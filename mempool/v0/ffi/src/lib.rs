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
    fn rsNotifyTxsAvailable();

    /// calls `mem.preCheck()` function in go.
    /// Note that the `tx` argument is cached in go
    /// Returns true if an error occured.
    fn rsMemPreCheck() -> bool;

    /// calls `mem.proxyAppConn.Error()` function in go.
    /// Returns true if an error occured.
    fn rsMemProxyAppConnError() -> bool;

    /// calls `mem.proxyAppConn.CheckTxAsync()` function in go, and can set the
    /// callback on the response object.
    /// Returns true if an error occured.
    fn rsMemProxyAppConnCheckTxAsync(set_callback: bool);
}

/// Rust's representation of go's MempoolConfig (just the parts we need) Note
/// that for simplicity, we use the widest types possible (e.g. i64 for unsigned
/// integers) to ensure compatibility between go and Rust on any system. There
/// might be a cleaner way to do it.
pub struct MempoolConfig {
    // Limit the total size of all txs in the mempool.
    // This only accounts for raw transactions (e.g. given 1MB transactions and
    // max_txs_bytes=5MB, mempool will only accept 5 transactions).
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
    // size of the sum of all txs the mempool, in bytes
    tx_bytes: i64,
    notified_txs_available: bool,
}

impl CListMempool {
    fn add_tx(&mut self, mem_tx: MempoolTx) {
        self.tx_bytes += mem_tx.tx.len() as i64;
        self.txs.insert(hash_tx(&mem_tx.tx), mem_tx);
    }

    fn remove_tx(&mut self, tx: &[u8]) {
        let tx_hash = hash_tx(tx);

        self.txs.remove(&tx_hash);
        self.tx_bytes -= tx.len() as i64;
    }

    fn size(&self) -> usize {
        self.txs.len()
    }

    fn is_full(&self, tx_size: i64) -> bool {
        let mem_size = self.txs.len() as i64;
        let mem_tx_bytes = self.tx_bytes;

        mem_size >= self.config.size || (mem_tx_bytes + tx_size) > self.config.max_tx_bytes
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

        unsafe { rsMemProxyAppConnCheckTxAsync(true) };

        false
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
        unsafe { rsNotifyTxsAvailable() };
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
            size,
            recheck,
        },
        height,
        txs: LinkedHashMap::new(),
        tx_bytes: 0,
        notified_txs_available: false,
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
        mempool.tx_bytes
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
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_remove_tx(
    _mempool_handle: Handle,
    tx: *const u8,
    tx_len: usize,
    _remove_from_cache: bool,
) {
    if let Some(ref mut mempool) = MEMPOOL {
        let tx = std::slice::from_raw_parts(tx, tx_len);
        mempool.remove_tx(tx);
    } else {
        panic!("Mempool not initialized!");
    }
}

/// Represents a raw transaction across the go/rust boundary.
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

#[repr(C)]
pub struct RawTxs {
    txs: *const RawTx,
    len: usize,
}

impl From<RawTxs> for Vec<&[u8]> {
    fn from(raw_txs: RawTxs) -> Self {
        let a = unsafe { std::slice::from_raw_parts(raw_txs.txs, raw_txs.len) };

        a.into_iter()
            .map(|raw_tx| unsafe { std::slice::from_raw_parts(raw_tx.tx, raw_tx.len) })
            .collect()
    }
}

/// Returned memory must not be stored in go. In go, use of `C.GoBytes()` is recommended.
/// Does not remove the transactions from the mempool.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_reap_max_bytes_max_gas(
    _mempool_handle: Handle,
    max_bytes: i64,
    max_gas: i64,
) -> RawTxs {
    if let Some(ref mempool) = MEMPOOL {
        let mut txs_to_return: Vec<&MempoolTx> = Vec::new();

        let mut running_size = 0;
        let mut running_gas = 0;
        for (_tx_hash, mem_tx) in mempool.txs.iter() {
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

        {
            let txs_to_return: Vec<_> = txs_to_return
                .into_iter()
                .map(|mem_tx| RawTx {
                    tx: mem_tx.tx.as_ptr(),
                    len: mem_tx.tx.len(),
                })
                .collect();

            RawTxs {
                txs: txs_to_return.as_ptr(),
                len: txs_to_return.len(),
            }
        }
    } else {
        panic!("Mempool not initialized!");
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

    mempool.height = height;
    mempool.notified_txs_available = true;

    let raw_txs: Vec<&[u8]> = raw_txs.into();

    for raw_tx in raw_txs {
        // Note: our implementation currently has no cache
        mempool.remove_tx(raw_tx);
    }

    if mempool.size() > 0 {
        if mempool.config.recheck {
            // TODO: recheckTxs
        } else {
            rsNotifyTxsAvailable();
        }
    }
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
pub unsafe extern "C" fn clist_mempool_free(_mempool_handle: Handle) {
    if let None = MEMPOOL {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Double-free detected!");
    }

    MEMPOOL = None;
}
