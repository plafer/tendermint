
use core::ffi::c_int;
use linked_hash_map::LinkedHashMap;
use std::collections::HashMap;

use crate::mempool::CListMempool;
use crate::mempool::MempoolConfig;
use crate::tx::MempoolTx;

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
    let mempool = if let Some(ref mempool) = MEMPOOL {
        mempool
    } else {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Mempool not initialized!");
    };

    mempool.size_bytes()
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
