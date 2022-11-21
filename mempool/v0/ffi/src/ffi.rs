use core::ffi::c_int;
use linked_hash_map::LinkedHashMap;
use std::collections::HashMap;

use crate::mempool::CListMempool;
use crate::mempool::MempoolConfig;
use crate::mempool::MempoolTx;

static mut MEMPOOL: Option<CListMempool> = None;

#[repr(C)]
pub struct Handle {
    handle: c_int,
}

/// Creates a new CListMempool.
/// Currently does not implement any cache.
///
/// # Safety
/// Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_new(
    max_tx_bytes: i64,
    max_txs_bytes: i64,
    size: i64,
    _keep_invalid_txs_in_cache: bool,
    recheck: bool,
    height: i64,
) -> Handle {
    if MEMPOOL.is_some() {
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

/// # Safety
/// Must be called by at most one thread at a time.
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

/// # Safety
/// Must be called by at most one thread at a time.
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

/// # Safety
/// Must be called by at most one thread at a time.
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

/// # Safety
/// + The fields in `raw_tx` must conform to the same requirements as `std::slice::from_raw_parts`.
/// + `raw_tx.ptr` must not be stored passed the return of this function
/// + Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_add_tx(
    _mempool_handle: Handle,
    height: i64,
    gas_wanted: i64,
    raw_tx: RawSlice,
) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let tx = std::slice::from_raw_parts(raw_tx.ptr, raw_tx.len);
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
///
/// # Safety
/// + The fields in `raw_tx` must conform to the same requirements as `std::slice::from_raw_parts`.
/// + `raw_tx.ptr` must not be stored passed the return of this function
/// + Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_remove_tx(
    _mempool_handle: Handle,
    raw_tx: RawSlice,
    _remove_from_cache: bool,
) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let tx = std::slice::from_raw_parts(raw_tx.ptr, raw_tx.len);
    mempool.remove_tx(tx);
}

/// # Safety
/// + The fields in `raw_tx_hash` must conform to the same requirements as `std::slice::from_raw_parts`.
/// + `raw_tx_hash.ptr` must not be stored passed the return of this function
/// + Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_remove_tx_by_key(
    _mempool_handle: Handle,
    raw_tx_hash: RawSlice,
) -> bool {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let tx_hash: &[u8] = raw_tx_hash.into();

    mempool.remove_tx_by_key(tx_hash)
}

/// # Safety
/// Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_tx_front(_mempool_handle: Handle) -> RawMempoolTx {
    let mempool = if let Some(ref mempool) = MEMPOOL {
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
///
/// # Safety
/// Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_reap_max_bytes_max_gas(
    _mempool_handle: Handle,
    max_bytes: i64,
    max_gas: i64,
) -> RustRawSlices {
    let mempool = if let Some(ref mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let txs_to_return: Vec<RawSlice> = mempool
        .reap_max_bytes_max_gas(max_bytes, max_gas)
        .into_iter()
        .map(|mem_tx| mem_tx.tx.as_slice().into())
        .collect();
    let num_txs_to_return = txs_to_return.len();
    let capacity = txs_to_return.capacity();

    RustRawSlices {
        ptr: txs_to_return.leak().as_ptr(),
        len: num_txs_to_return,
        capacity,
    }
}

/// # Safety
/// Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_reap_max_txs(
    _mempool_handle: Handle,
    max_txs: i32,
) -> RustRawSlices {
    let mempool = if let Some(ref mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };
    let txs_to_return: Vec<RawSlice> = mempool
        .reap_max_txs(max_txs)
        .into_iter()
        .map(|mem_tx| mem_tx.tx.as_slice().into())
        .collect();
    let num_txs_to_return = txs_to_return.len();
    let capacity = txs_to_return.capacity();

    RustRawSlices {
        ptr: txs_to_return.leak().as_ptr(),
        len: num_txs_to_return,
        capacity,
    }
}

/// # Safety
/// + The fields in `raw_txs`, and each inner `RawSlice`, must conform to the same requirements as `std::slice::from_raw_parts`.
/// + `raw_txs.ptr`, and all inner `RawSlice.ptr`, must not be stored passed the return of this function
/// + Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_update(
    _mempool_handle: Handle,
    height: i64,
    raw_txs: GoRawSlices,
) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    let raw_txs: Vec<&[u8]> = raw_txs.into();
    mempool.update(height, raw_txs.as_slice());
}

/// # Safety
/// Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_flush(_mempool_handle: Handle) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    mempool.flush();
}

/// # Safety
/// + The fields in `raw_tx` must conform to the same requirements as `std::slice::from_raw_parts`
/// + `raw_tx.ptr` must not be stored passed the return of this function
/// + Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_check_tx(_mempool_handle: Handle, raw_tx: RawSlice) -> bool {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    mempool.check_tx(raw_tx.into())
}

/// # Safety
/// + The fields in `raw_tx` must conform to the same requirements as `std::slice::from_raw_parts`
/// + `raw_tx.ptr` must not be stored passed the return of this function
/// + Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_res_cb_first_time(
    _mempool_handle: Handle,
    check_tx_code: u32,
    has_post_check_err: bool,
    raw_tx: RawSlice,
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

/// # Safety
/// + The fields in `raw_tx` must conform to the same requirements as `std::slice::from_raw_parts`
/// + `raw_tx.ptr` must not be stored passed the return of this function
/// + Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_res_cb_recheck(
    _mempool_handle: Handle,
    check_tx_code: u32,
    raw_tx: RawSlice,
) {
    let mempool = if let Some(ref mut mempool) = MEMPOOL {
        mempool
    } else {
        panic!("Mempool not initialized!");
    };

    mempool.res_cb_recheck(check_tx_code, raw_tx.into())
}

/// # Safety
/// Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_free(_mempool_handle: Handle) {
        if MEMPOOL.is_none() {
            // Panicking across an FFI boundary is undefined behavior. However,
            // it'll have to do for this proof of concept :).
            panic!("Double-free detected!");
        }

        MEMPOOL = None;
}

/// Represents a borrowed slice across the go/rust boundary.
/// It must not be stored by the callee passed the return of the function call.
#[repr(C)]
pub struct RawSlice {
    ptr: *const u8,
    len: usize,
}

impl From<RawSlice> for &[u8] {
    fn from(raw_tx: RawSlice) -> Self {
        unsafe { std::slice::from_raw_parts(raw_tx.ptr, raw_tx.len) }
    }
}

impl From<&[u8]> for RawSlice {
    fn from(tx: &[u8]) -> Self {
        RawSlice {
            ptr: tx.as_ptr(),
            len: tx.len(),
        }
    }
}

/// Transactions that were allocated and owned by Go. Note that only the
/// top-level collection (at location `ptr`) is guaranteed to be owned by Go;
/// this data structure makes no claim about the ownership of each individual
/// `RawSlice`
#[repr(C)]
pub struct GoRawSlices {
    ptr: *const RawSlice,
    len: usize,
}

impl From<GoRawSlices> for Vec<&[u8]> {
    fn from(raw_txs: GoRawSlices) -> Self {
        let a = if raw_txs.ptr.is_null() {
            &[]
        } else {
            unsafe { std::slice::from_raw_parts(raw_txs.ptr, raw_txs.len) }
        };

        a.iter()
            .map(|raw_tx| unsafe { std::slice::from_raw_parts(raw_tx.ptr, raw_tx.len) })
            .collect()
    }
}

/// Transactions that were allocated and owned by Rust.
/// Must be deallocated from Go with `clist_mempool_raw_slices_free`.
#[repr(C)]
pub struct RustRawSlices {
    ptr: *const RawSlice,
    len: usize,
    capacity: usize,
}

/// # Safety
/// + The fields in `raw_txs` must conform to the same requirements as `std::vec::Vec::from_raw_parts`
/// + `raw_txs.ptr`, and all inner `RawSlice.ptr`, must not be stored passed the return of this function
/// + Must be called by at most one thread at a time.
#[no_mangle]
pub unsafe extern "C" fn clist_mempool_raw_slices_free(
    _mempool_handle: Handle,
    raw_txs: RustRawSlices,
) {
    let _vec = Vec::from_raw_parts(raw_txs.ptr.cast_mut(), raw_txs.len, raw_txs.capacity);

    // _vec dropped and memory freed
}

/// Raw representation of a `MempoolTx`.
#[repr(C)]
pub struct RawMempoolTx {
    pub height: i64,
    pub gas_wanted: i64,
    /// Owned by Rust. This should be thought of as a view in the mempool.
    pub raw_tx: RawSlice,
    // Owned by Go; should be freed by calling `clist_mempool_raw_mempool_free()`
    pub senders: *const u16,
    pub senders_len: usize,
    /// Necessary to be able to appropriately cleanup `RawMempoolTx`
    pub senders_capacity: usize,
}

impl RawMempoolTx {
    /// Constructs the null representation
    fn null() -> RawMempoolTx {
        RawMempoolTx {
            height: 0,
            gas_wanted: 0,
            raw_tx: RawSlice {
                ptr: std::ptr::null(),
                len: 0,
            },
            senders: std::ptr::null(),
            senders_len: 0,
            senders_capacity: 00,
        }
    }
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

/// # Safety
/// + The `senders*` fields in `raw_mem_tx` must conform to the same requirements as `std::vec::Vec::from_raw_parts`
/// + The fields in `raw_mem_tx.raw_tx` must conform to the same requirements as `std::slice::from_raw_parts`.
/// + No pointer must be stored passed the return of this function
/// + Must be called by at most one thread at a time.
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
