use sha2::{digest::generic_array::{GenericArray, typenum::U32}, Sha256, Digest};

/// Type that key hashes have
pub type TxKeyHash = GenericArray<u8, U32>;

pub struct MempoolTx {
    pub height: i64,
    pub gas_wanted: i64,
    pub tx: Vec<u8>,
    // also (add later)
    // senders: PeerId -> bool
}

impl MempoolTx {
    pub fn hash(&self) -> TxKeyHash {
        let mut hasher = Sha256::new();
        hasher.update(&self.tx);
        hasher.finalize()
    }
}
