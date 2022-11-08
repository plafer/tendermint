///! Rust implementation of the CList mempool
///! The current implementation only supports one mempool instantiated.
///! However, the handle-based API is designed to support arbitrarily many.
///! Every function exposed is expected to be accessed be 1 thread at a time.
///! In other words, the go code is expected to guard the access to the FFI
///! with a mutex.
mod tx;

use core::ffi::c_int;

use linked_hash_map::LinkedHashMap;
use tx::{hash_tx, MempoolTx, TxKeyHash};

extern "C" {
    fn rsNotifyTxsAvailable();
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
    fn remove_tx(&mut self, tx: &[u8]) {
        let tx_hash = hash_tx(tx);

        self.txs.remove(&tx_hash);
        self.tx_bytes -= tx.len() as i64;
    }

    fn size(&self) -> usize {
        self.txs.len()
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
    if let Some(ref mempool) = MEMPOOL {
        mempool.size()
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
    if let Some(ref mut mempool) = MEMPOOL {
        let tx = std::slice::from_raw_parts(tx, tx_len);
        let tx_vec: Vec<u8> = {
            let mut tx_vec = Vec::with_capacity(tx_len);
            tx_vec.extend_from_slice(tx);
            tx_vec
        };
        let mempool_tx = MempoolTx {
            height,
            gas_wanted,
            tx: tx_vec,
        };
        mempool.txs.insert(hash_tx(&mempool_tx.tx), mempool_tx);

        mempool.tx_bytes += tx_len as i64;
    } else {
        panic!("Mempool not initialized!");
    }
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
pub unsafe extern "C" fn clist_mempool_free(_mempool_handle: Handle) {
    if let None = MEMPOOL {
        // Panicking across an FFI boundary is undefined behavior. However,
        // it'll have to do for this proof of concept :).
        panic!("Double-free detected!");
    }

    MEMPOOL = None;
}
