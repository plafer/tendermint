///! Rust implementation of the CList mempool
///! The current implementation only supports one mempool instantiated.
///! However, the handle-based API is designed to support arbitrarily many.
///! Every function exposed is expected to be accessed be 1 thread at a time.
///! In other words, the go code is expected to guard the access to the FFI
///! with a mutex.
use core::ffi::c_int;
use std::collections::VecDeque;

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
    // Do not remove invalid transactions from the cache (default: false)
    // Set to true if it's not possible for any invalid transaction to become
    // valid again in the future.
    keep_invalid_txs_in_cache: bool,
    recheck: bool,
}

struct MempoolTx {
    height: i64,
    gas_wanted: i64,
    tx: Vec<u8>,
    // also (add later)
    // senders: PeerId -> bool
}

pub struct CListMempool {
    config: MempoolConfig,
    height: i64,
    txs: VecDeque<MempoolTx>,
    // size of the sum of all txs the mempool, in bytes
    tx_bytes: i64,
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
    keep_invalid_txs_in_cache: bool,
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
            keep_invalid_txs_in_cache,
            recheck,
        },
        height,
        txs: VecDeque::new(),
        tx_bytes: 0,
    });

    Handle { handle: 0 }
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_size(_mempool_handle: Handle) -> usize {
    if let Some(ref mempool) = MEMPOOL {
        mempool.txs.len()
    } else {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Mempool not initialized!");
    }
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
    if let Some(ref mempool) = MEMPOOL {
        let mem_size = mempool.txs.len() as i64;
        let mem_tx_bytes = mempool.tx_bytes;

        mem_size >= mempool.config.size || (mem_tx_bytes + tx_size) > mempool.config.max_tx_bytes
    } else {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Mempool not initialized!");
    }
}

#[no_mangle]
pub unsafe extern "C" fn clist_mempool_add_tx(_mempool_handle: Handle, height: i64, gas_wanted: i64, tx: *mut u8) -> bool {
    todo!()
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
