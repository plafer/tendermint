use std::collections::HashMap;

use linked_hash_map::LinkedHashMap;

use crate::tx::{hash_tx, MempoolTx, PeerId, TxKeyHash};
use crate::RawSlice;

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
    fn rsMemPostCheck(tx: RawSlice) -> bool;

    /// calls `mem.proxyAppConn.Error()` function in go.
    /// Returns true if an error occured.
    fn rsMemProxyAppConnError() -> bool;

    /// calls `mem.proxyAppConn.CheckTxAsync()` function in go, and can set the
    /// callback on the response object.
    /// Returns true if an error occured.
    fn rsMemProxyAppConnCheckTxAsync(raw_tx: RawSlice, set_callback: bool);

    /// calls `mem.proxyAppConn.FlushAsync()` function in go
    fn rsMemProxyAppConnFlushAsync();

    /// closes the transaction wait channel, which unblocks any goroutine
    /// that was waiting for new transactions
    fn rsCloseTxsWaitChan();

    /// opens the transaction wait channel, which blocks any goroutine that
    /// needs txs from the mempool
    fn rsMakeTxsWaitChan();

    fn rsComputeProtoSizeForTx(raw_tx: RawSlice) -> i64;
}

/// Rust's representation of go's MempoolConfig (just the parts we need) Note
/// that for simplicity, we use the widest types possible (e.g. i64 for unsigned
/// integers) to ensure compatibility between go and Rust on any system. There
/// might be a cleaner way to do it.
pub struct MempoolConfig {
    // Limit the total size of all txs in the mempool.
    // This only accounts for raw transactions (e.g. given 1MB transactions and
    // max_txs_bytes=5MB, mempool will only accept 5 transactions).
    pub max_txs_bytes: i64,
    ///	Maximum size of a single transaction
    /// NOTE: the max size of a tx transmitted over the network is {max_tx_bytes}.
    pub max_tx_bytes: i64,
    /// Maximum number of txs in the mempool
    pub size: i64,
    pub recheck: bool,
}

pub struct CListMempool {
    pub config: MempoolConfig,
    pub height: i64,
    /// Implements both go's `tx` and `txsMap`
    pub txs: LinkedHashMap<TxKeyHash, MempoolTx>,
    /// size of the sum of all txs the mempool, in bytes
    pub txs_bytes: i64,
    pub notified_txs_available: bool,

    /// List of transactions left to recheck after an `update()`.
    /// This is done asynchronously over multiple calls.
    pub recheck_txs: Vec<MempoolTx>,
}

impl CListMempool {
    pub fn add_tx(&mut self, mem_tx: MempoolTx) {
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

    pub fn remove_tx(&mut self, tx: &[u8]) {
        let tx_hash = hash_tx(tx);

        self.txs.remove(&tx_hash);
        self.txs_bytes -= tx.len() as i64;

        if self.txs.is_empty() {
            // if we removed the last item, make "customer goroutines" wait
            unsafe { rsMakeTxsWaitChan() };
        }
    }

    /// Returns true if there was an error (the tx was not found)
    pub fn remove_tx_by_key(&mut self, tx_hash: &[u8]) -> bool {
        if self.txs.contains_key(tx_hash) == false {
            return true;
        }

        // `unwrap()` is safe because we made sure that the key exists
        // FIXME: clean this up
        let mem_tx = self.txs.get(tx_hash).unwrap();
        self.remove_tx(mem_tx.tx.clone().as_slice());

        false
    }

    pub fn size(&self) -> usize {
        self.txs.len()
    }

    pub fn size_bytes(&self) -> i64 {
        self.txs_bytes
    }

    pub fn is_full(&self, tx_size: i64) -> bool {
        let mem_size = self.txs.len() as i64;
        let mem_tx_bytes = self.txs_bytes;

        mem_size >= self.config.size || (mem_tx_bytes + tx_size) > self.config.max_txs_bytes
    }

    /// Returns the first tx in the mempool, or `None` if the mempool is empty
    pub fn front(&self) -> Option<&MempoolTx> {
        self.txs.front().map(|(_tx_hash, tx)| tx)
    }

    /// A return of `true` means that an error occured. Normally we would want
    /// proper error reporting across the ffi boundary.
    pub fn check_tx(&self, tx: &[u8]) -> bool {
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

    pub fn update(&mut self, height: i64, raw_txs: &[&[u8]]) {
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

    pub fn reap_max_bytes_max_gas(&self, max_bytes: i64, max_gas: i64) -> Vec<&MempoolTx> {
        let mut txs_to_return: Vec<&MempoolTx> = Vec::new();

        let mut running_size = 0;
        let mut running_gas = 0;
        for (_tx_hash, mem_tx) in self.txs.iter() {
            let temptative_size = {
                let tx_proto_size = unsafe { rsComputeProtoSizeForTx(mem_tx.tx.as_slice().into()) };
                running_size + tx_proto_size
            };
            
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

    pub fn reap_max_txs(&self, max_txs: i32) -> Vec<&MempoolTx> {
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
    pub fn flush(&mut self) {
        self.txs_bytes = 0;
        self.txs.clear();

        unsafe { rsMakeTxsWaitChan() };
    }

    /// impl of resCbFirstTime after the postCheck function is called in go
    pub fn res_cb_first_time(
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

    pub fn res_cb_recheck(&mut self, check_tx_code: u32, raw_tx: &[u8]) {
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

    pub fn notify_txs_available(&mut self) {
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
