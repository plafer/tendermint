
pub struct MempoolTx {
    pub height: i64,
    pub gas_wanted: i64,
    pub tx: Vec<u8>,
    // also (add later)
    // senders: PeerId -> bool
}
