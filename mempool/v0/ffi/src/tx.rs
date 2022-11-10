use std::collections::HashMap;

use sha2::{
    digest::generic_array::{typenum::U32, GenericArray},
    Digest, Sha256,
};

/// Type that key hashes have
pub type TxKeyHash = GenericArray<u8, U32>;

#[derive(Hash, PartialEq, Eq)]
pub struct PeerId(pub u16);

pub struct MempoolTx {
    pub height: i64,
    pub gas_wanted: i64,
    pub tx: Vec<u8>,
    pub senders: HashMap<PeerId, bool>,
}

// TODO: Investigate using std::hash instead
pub fn hash_tx(tx: &[u8]) -> TxKeyHash {
    let mut hasher = Sha256::new();
    hasher.update(tx);
    hasher.finalize()
}
