///! Rust implementation of the CList mempool
///! The current implementation only supports one mempool instantiated.
///! However, the handle-based API is designed to support arbitrarily many.
///! Every function exposed is expected to be accessed be 1 thread at a time.
///! In other words, the go code is expected to guard the access to the FFI
///! with a mutex.
mod tx;

use core::ffi::c_int;
use std::collections::HashMap;

use linked_hash_map::LinkedHashMap;
use tx::{hash_tx, MempoolTx, PeerId, TxKeyHash};

const ABCI_CODE_TYPE_OK: u32 = 0;

extern "C" {
    /// Tests whether the `txsAvailable` feature is enabled
    fn rsTxsAvailableEnabled() -> bool;

    /// Writes to the `txsAvailable` channel to notify
    /// any goroutine waiting on new transactions
    fn rsNotifyTxsAvailable();

    /// calls `mem.preCheck()` function in go.
    /// Note that the `tx` argument is cached in go
    /// Returns true if an error occured.
    fn rsMemPreCheck() -> bool;

    /// calls `mem.postCheck()` function in go.
    /// Returns true if an error occured.
    fn rsMemPostCheck(tx: RawTx) -> bool;

    /// calls `mem.proxyAppConn.Error()` function in go.
    /// Returns true if an error occured.
    fn rsMemProxyAppConnError() -> bool;

    /// calls `mem.proxyAppConn.CheckTxAsync()` function in go, and can set the
    /// callback on the response object.
    /// Returns true if an error occured.
    fn rsMemProxyAppConnCheckTxAsync(raw_tx: RawTx, set_callback: bool);

    /// calls `mem.proxyAppConn.FlushAsync()` function in go
    fn rsMemProxyAppConnFlushAsync();

    /// closes the transaction wait channel, which unblocks any goroutine
    /// that was waiting for new transactions
    fn rsCloseTxsWaitChan();

    /// opens the transaction wait channel, which blocks any goroutine that
    /// needs txs from the mempool
    fn rsMakeTxsWaitChan();
}

/// Rust's representation of go's MempoolConfig (just the parts we need) Note
/// that for simplicity, we use the widest types possible (e.g. i64 for unsigned
/// integers) to ensure compatibility between go and Rust on any system. There
/// might be a cleaner way to do it.
pub struct MempoolConfig {
    // Limit the total size of all txs in the mempool.
    // This only accounts for raw transactions (e.g. given 1MB transactions and
    // max_txs_bytes=5MB, mempool will only accept 5 transactions).
    max_txs_bytes: i64,
    ///	Maximum size of a single transaction
    /// NOTE: the max size of a tx transmitted over the network is {max_tx_bytes}.
    max_tx_bytes: i64,
    /// Maximum number of txs in the mempool
    size: i64,
    recheck: bool,
}

pub struct CListMempool {
    config: MempoolConfig,
    height: i64,
    /// Implements both go's `tx` and `txsMap`
    txs: LinkedHashMap<TxKeyHash, MempoolTx>,
    /// size of the sum of all txs the mempool, in bytes
    txs_bytes: i64,
    notified_txs_available: bool,

    /// List of transactions left to recheck after an `update()`.
    /// This is done asynchronously over multiple calls.
    recheck_txs: Vec<MempoolTx>,
}

impl CListMempool {
    fn add_tx(&mut self, mem_tx: MempoolTx) {
        // Our implementation differs slightly than Go's. Actually tendermint's `txsMap` is
        // slightly broken when no cache is present, and we don't reproduce that here.
        // In the go canonical go implementation, without a cache, one can add the same tx twice to the mempool.
        // However, the `txsMap` only points to the last item. Here, we prevent the
        // tx to be added twice.
        let tx_hash = hash_tx(&mem_tx.tx);
        if self.txs.contains_key(&tx_hash) {
            return;
        }
        
        self.txs_bytes += mem_tx.tx.len() as i64;
        self.txs.insert(tx_hash, mem_tx);

        if self.size() == 1 {
            // if we just added the first item, unblock any "customer goroutines"
            // that may be waiting
            unsafe { rsCloseTxsWaitChan() };
        }
    }

    fn remove_tx(&mut self, tx: &[u8]) {
        let tx_hash = hash_tx(tx);

        self.txs.remove(&tx_hash);
        self.txs_bytes -= tx.len() as i64;

        if self.txs.is_empty() {
            // if we removed the last item, make "customer goroutines" wait
            unsafe { rsMakeTxsWaitChan() };
        }
    }

    /// Returns true if there was an error (the tx was not found)
    fn remove_tx_by_key(&mut self, tx_hash: &[u8]) -> bool {
        if self.txs.contains_key(tx_hash) == false {
            return true;
        }

        // `unwrap()` is safe because we made sure that the key exists
        // FIXME: clean this up
        let mem_tx = self.txs.get(tx_hash).unwrap();
        self.remove_tx(mem_tx.tx.clone().as_slice());

        false
    }

    fn size(&self) -> usize {
        self.txs.len()
    }

    fn is_full(&self, tx_size: i64) -> bool {
        let mem_size = self.txs.len() as i64;
        let mem_tx_bytes = self.txs_bytes;

        mem_size >= self.config.size || (mem_tx_bytes + tx_size) > self.config.max_txs_bytes
    }

    /// Returns the first tx in the mempool, or `None` if the mempool is empty
    fn front(&self) -> Option<&MempoolTx> {
        self.txs.front().map(|(_tx_hash, tx)| tx)
    }

    /// A return of `true` means that an error occured. Normally we would want
    /// proper error reporting across the ffi boundary.
    fn check_tx(&self, tx: &[u8]) -> bool {
        let tx_size = tx.len() as i64;
        if self.is_full(tx_size) {
            return true;
        }

        if tx_size > self.config.max_tx_bytes {
            // tx too large
            return true;
        }

        if unsafe { rsMemPreCheck() } {
            return true;
        }

        if unsafe { rsMemProxyAppConnError() } {
            return true;
        }

        // Sets `res_cb_first_time()` as callback
        unsafe { rsMemProxyAppConnCheckTxAsync(tx.into(), true) };

        false
    }

    fn update(&mut self, height: i64, raw_txs: &[&[u8]]) {
        self.height = height;
        self.notified_txs_available = false;

        for raw_tx in raw_txs {
            // Note: our implementation currently has no cache
            self.remove_tx(raw_tx);
        }

        if self.size() > 0 {
            if self.config.recheck {
                // FIXME: I don't think we have any guarantee we'll never hit this
                assert!(self.recheck_txs.is_empty());
                self.recheck_txs
                    .extend(self.txs.iter().map(|(_, mem_tx)| mem_tx.clone()));

                // NOTE (from original implementation): globalCb may be called concurrently
                // TODO: make sure this is legal in this impl
                // try: have `globalCb` take the `updateMtx` (which is held by caller when this runs)
                // make sure we don't deadlock though.
                for (_, mem_tx) in self.txs.iter() {
                    let raw_tx = mem_tx.tx.as_slice().into();
                    unsafe { rsMemProxyAppConnCheckTxAsync(raw_tx, false) };
                }
                unsafe { rsMemProxyAppConnFlushAsync() };
            } else {
                self.notify_txs_available();
            }
        }
    }

    fn reap_max_bytes_max_gas(&self, max_bytes: i64, max_gas: i64) -> Vec<&MempoolTx> {
        let mut txs_to_return: Vec<&MempoolTx> = Vec::new();

        let mut running_size = 0;
        let mut running_gas = 0;
        for (_tx_hash, mem_tx) in self.txs.iter() {
            // FIXME: this is incorrect. We need to look at the size of all the
            // current txs once marshalled to protobuf format
            let temptative_size = running_size + mem_tx.tx.len() as i64;
            if max_bytes > -1 && temptative_size > max_bytes {
                break;
            }

            let temptative_gas = running_gas + mem_tx.gas_wanted;
            if max_gas > -1 && temptative_gas > max_gas {
                break;
            }

            // success: we can add the current tx to the current output
            txs_to_return.push(mem_tx);
            running_size = temptative_size;
            running_gas = temptative_gas;
        }

        txs_to_return
    }

    fn reap_max_txs(&self, max_txs: i32) -> Vec<&MempoolTx> {
        let num_txs_to_return = if max_txs < 0 {
            self.size()
        } else {
            // guaranteed not to panic, since we made sure `max >= 0`
            max_txs.try_into().unwrap()
        };

        self.txs
            .iter()
            .take(num_txs_to_return)
            .map(|(_, mem_tx)| mem_tx)
            .collect()
    }

    /// (from go) XXX: Unsafe! Calling Flush may leave mempool in inconsistent state.
    /// Only used in tests
    fn flush(&mut self) {
        self.txs_bytes = 0;
        self.txs.clear();

        unsafe { rsMakeTxsWaitChan() };
    }

    /// impl of resCbFirstTime after the postCheck function is called in go
    fn res_cb_first_time(
        &mut self,
        check_tx_code: u32,
        has_post_check_err: bool,
        raw_tx: &[u8],
        gas_wanted: i64,
        peer_id: u16,
    ) {
        if self.recheck_txs.is_empty() == false {
            panic!("recheck_txs not empty. This should never happen.");
        }

        if check_tx_code != ABCI_CODE_TYPE_OK || has_post_check_err {
            // We currently don't log or maintain metrics, nor have a cache, so nothing to do here
            return;
        }

        if self.is_full(raw_tx.len() as i64) {
            return;
        }

        let mem_tx = MempoolTx {
            height: self.height,
            gas_wanted,
            tx: Vec::from(raw_tx),
            senders: {
                let mut hash_map = HashMap::new();
                hash_map.insert(PeerId(peer_id), true);
                hash_map
            },
        };

        self.add_tx(mem_tx);
        self.notify_txs_available();
    }

    fn res_cb_recheck(&mut self, check_tx_code: u32, raw_tx: &[u8]) {
        // Equivalent to the previous `mem.recheckCursor == nil` check
        // in go's `globalCb`
        if self.recheck_txs.is_empty() {
            return;
        }

        while let Some(next_mem_tx) = self.recheck_txs.pop() {
            // FIXME: Improvement: store hashes only in `self.recheck_txs`
            if next_mem_tx.tx.as_slice() == raw_tx {
                break;
            }

            // Txs didn't match, move on to the next one
            println!("re-Check transaction mismatch.");
        }

        let has_post_check_error = unsafe { rsMemPostCheck(raw_tx.into()) };

        if (check_tx_code == ABCI_CODE_TYPE_OK) && !has_post_check_error {
            // nothing to do
        } else {
            self.remove_tx(raw_tx);
        }

        if self.size() > 0 {
            self.notify_txs_available();
        }
    }

    fn notify_txs_available(&mut self) {
        if self.size() == 0 {
            // Note: We're not supposed to panic across FFI boundary.
            panic!("notified txs available but mempool is empty!")
        }

        if unsafe { rsTxsAvailableEnabled() } && self.notified_txs_available == false {
            self.notified_txs_available = true;
            unsafe { rsNotifyTxsAvailable() };
        }
    }
}

static mut MEMPOOL: Option<CListMempool> = None;

#[repr(C)]
pub struct Handle {
    handle: c_int,
}

/// Creates a new CListMempool.
/// Currently does not implement any cache.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_new(
    max_tx_bytes: i64,
    max_txs_bytes: i64,
    size: i64,
    _keep_invalid_txs_in_cache: bool,
    recheck: bool,
    height: i64,
) -> Handle {
    if let Some(_) = MEMPOOL {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("oops, only one mempool supported at the moment");
    }

    MEMPOOL = Some(CListMempool {
        config: MempoolConfig {
            max_tx_bytes,
            max_txs_bytes,
            size,
            recheck,
        },
        height,
        txs: LinkedHashMap::new(),
        txs_bytes: 0,
        notified_txs_available: false,
        recheck_txs: Vec::new(),
    });

    Handle { handle: 0 }
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_size(_mempool_handle: Handle) -> usize {
    let mempool = if let Some(ref mempool) = MEMPOOL {
        mempool
    } else {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Mempool not initialized!");
    };

    mempool.size()
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_size_bytes(_mempool_handle: Handle) -> i64 {
    if let Some(ref mempool) = MEMPOOL {
        mempool.txs_bytes
    } else {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Mempool not initialized!");
    }
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_is_full(_mempool_handle: Handle, tx_size: i64) -> bool {
    let mempool = if let Some(ref mempool) = MEMPOOL {
        mempool
    } else {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Mempool not initialized!");
    };

    mempool.is_full(tx_size)
}

/// `tx` must not be stored by the Rust code
/// TODO: use `RawTx`
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_add_tx(
    _mempool_handle: Handle,
    height: i64,
    gas_wanted: i64,
    tx: *const u8,
    tx_len: usize,
) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let tx = std::slice::from_raw_parts(tx, tx_len);
    let tx = Vec::from(tx);

    let mempool_tx = MempoolTx {
        height,
        gas_wanted,
        tx,
        senders: HashMap::new(),
    };

    mempool.add_tx(mempool_tx);
}

/// `tx` must not be stored by the Rust code
/// Note: I think this will eventually go, as all calls
/// remove a tx will be coming from rust.
/// FIXME: Use `RawTx` here instead of `tx` and `tx_len`
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_remove_tx(
    _mempool_handle: Handle,
    tx: *const u8,
    tx_len: usize,
    _remove_from_cache: bool,
) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let tx = std::slice::from_raw_parts(tx, tx_len);
    mempool.remove_tx(tx);
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_remove_tx_by_key(
    _mempool_handle: Handle,
    raw_tx_hash: RawTx,
) -> bool {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let tx_hash: &[u8] = raw_tx_hash.into();

    mempool.remove_tx_by_key(tx_hash)
}

/// Represents a raw transaction across the go/rust boundary.
/// FIXME: Rename to `RawSlice`
#[repr(C)]
pub struct RawTx {
    tx: *const u8,
    len: usize,
}

impl From<RawTx> for &[u8] {
    fn from(raw_tx: RawTx) -> Self {
        unsafe { std::slice::from_raw_parts(raw_tx.tx, raw_tx.len) }
    }
}

impl From<&[u8]> for RawTx {
    fn from(tx: &[u8]) -> Self {
        RawTx {
            tx: tx.as_ptr(),
            len: tx.len(),
        }
    }
}

/// Transactions that were allocated and owned by Go.
/// TODO: Rename to `GoRawTxs`
#[repr(C)]
pub struct RawTxs {
    txs: *const RawTx,
    len: usize,
}

impl From<RawTxs> for Vec<&[u8]> {
    fn from(raw_txs: RawTxs) -> Self {
        let a = if raw_txs.txs == std::ptr::null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(raw_txs.txs, raw_txs.len) }
        };

        a.into_iter()
            .map(|raw_tx| unsafe { std::slice::from_raw_parts(raw_tx.tx, raw_tx.len) })
            .collect()
    }
}

/// Transactions that were allocated and owned by Rust.
/// Must be deallocated from Go with `clist_raw_txs_free`.
#[repr(C)]
pub struct RustRawTxs {
    txs: *const RawTx,
    len: usize,
    capacity: usize,
}

/// Raw representation of a `MempoolTx`.
#[repr(C)]
pub struct RawMempoolTx {
    pub height: i64,
    pub gas_wanted: i64,
    /// Owned by Rust. This should be thought of as a view in the mempool.
    pub raw_tx: RawTx,
    // Owned by Go; should be freed by calling `clist_mempool_raw_mempool_free()`
    pub senders: *const u16,
    pub senders_len: usize,
    /// Necessary to be able to appropriately cleanup `RawMempoolTx`
    pub senders_capacity: usize,
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_raw_txs_free(_mempool_handle: Handle, raw_txs: RustRawTxs) {
    let _vec = Vec::from_raw_parts(raw_txs.txs.cast_mut(), raw_txs.len, raw_txs.capacity);

    // _vec dropped and memory freed
}

impl From<&MempoolTx> for RawMempoolTx {
    fn from(mem_tx: &MempoolTx) -> Self {
        let senders: Vec<u16> = mem_tx.senders.keys().map(|peer_id| peer_id.0).collect();
        let senders_len = senders.len();
        let senders_capacity = senders.capacity();

        Self {
            height: mem_tx.height,
            gas_wanted: mem_tx.gas_wanted,
            raw_tx: mem_tx.tx.as_slice().into(),
            senders: senders.leak().as_mut_ptr(),
            senders_len,
            senders_capacity,
        }
    }
}

impl RawMempoolTx {
    /// Constructs the null representation
    fn null() -> RawMempoolTx {
        RawMempoolTx {
            height: 0,
            gas_wanted: 0,
            raw_tx: RawTx {
                tx: std::ptr::null(),
                len: 0,
            },
            senders: std::ptr::null(),
            senders_len: 0,
            senders_capacity: 00,
        }
    }
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_raw_mempool_tx_free(raw_mem_tx: RawMempoolTx) {
    let _vec = Vec::from_raw_parts(
        raw_mem_tx.senders.cast_mut(),
        raw_mem_tx.senders_len,
        raw_mem_tx.senders_capacity,
    );

    // Note: we don't free `raw_mem_tx` as it still exists in the mempool.
    // It gets cleaned up in an `update()`
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_tx_front(_mempool_handle: Handle) -> RawMempoolTx {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    match mempool.front() {
        Some(mem_tx) => mem_tx.into(),
        None => RawMempoolTx::null(),
    }
}
/// Returned memory must not be stored in go.
/// In go, use of `C.GoBytes()` is recommended.
/// Does not remove the transactions from the mempool.
/// Returned transactions will now be owned by Go and must be freed using
/// `clist_mempool_raw_free()`
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_reap_max_bytes_max_gas(
    _mempool_handle: Handle,
    max_bytes: i64,
    max_gas: i64,
) -> RustRawTxs {
    let mempool = if let Some(ref mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let txs_to_return: Vec<RawTx> = mempool
        .reap_max_bytes_max_gas(max_bytes, max_gas)
        .into_iter()
        .map(|mem_tx| mem_tx.tx.as_slice().into())
        .collect();
    let num_txs_to_return = txs_to_return.len();
    let capacity = txs_to_return.capacity();

    RustRawTxs {
        txs: txs_to_return.leak().as_ptr(),
        len: num_txs_to_return,
        capacity,
    }
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_reap_max_txs(
    _mempool_handle: Handle,
    max_txs: i32,
) -> RustRawTxs {
    let mempool = if let Some(ref mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };
    let txs_to_return: Vec<RawTx> = mempool
        .reap_max_txs(max_txs)
        .into_iter()
        .map(|mem_tx| mem_tx.tx.as_slice().into())
        .collect();
    let num_txs_to_return = txs_to_return.len();
    let capacity = txs_to_return.capacity();

    RustRawTxs {
        txs: txs_to_return.leak().as_ptr(),
        len: num_txs_to_return,
        capacity,
    }
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_update(
    _mempool_handle: Handle,
    height: i64,
    raw_txs: RawTxs,
) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let raw_txs: Vec<&[u8]> = raw_txs.into();
    mempool.update(height, raw_txs.as_slice());
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_flush(_mempool_handle: Handle) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    mempool.flush();
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_check_tx(_mempool_handle: Handle, raw_tx: RawTx) -> bool {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    mempool.check_tx(raw_tx.into())
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_res_cb_first_time(
    _mempool_handle: Handle,
    check_tx_code: u32,
    has_post_check_err: bool,
    raw_tx: RawTx,
    gas_wanted: i64,
    peer_id: u16,
) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    mempool.res_cb_first_time(
        check_tx_code,
        has_post_check_err,
        raw_tx.into(),
        gas_wanted,
        peer_id,
    )
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_res_cb_recheck(
    _mempool_handle: Handle,
    check_tx_code: u32,
    raw_tx: RawTx,
) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    mempool.res_cb_recheck(check_tx_code, raw_tx.into())
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_free(_mempool_handle: Handle) {
    if let None = MEMPOOL {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Double-free detected!");
    }

    MEMPOOL = None;
}
