///! Rust implementation of the CList mempool
///! The current implementation only supports one mempool instantiated.
///! However, the handle-based API is designed to support arbitrarily many.
///! Every function exposed is expected to be accessed be 1 thread at a time.
///! In other words, the go code is expected to guard the access to the FFI
///! with a mutex.
use core::ffi::c_int;

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

pub struct CListMempool {
    config: MempoolConfig,
    height: i64,
}

static mut MEMPOOL: Option<CListMempool> = None;

#[repr(C)]
pub struct Handle {
    handle: c_int,
}

/// Creates a new CListMempool.
/// Currently does not implement any cache.
#[no_mangle]
pub unsafe extern "C" fn CListMempool_new(
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
    });

    Handle { handle: 0 }
}

#[no_mangle]
pub unsafe extern "C" fn CListMempool_free(_mempool_handle: Handle) {
    if let None = MEMPOOL {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Double-free detected!");
    }

    MEMPOOL = None;
}
