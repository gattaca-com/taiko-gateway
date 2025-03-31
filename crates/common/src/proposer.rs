use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use alloy_consensus::TxEnvelope;
use alloy_primitives::{Address, Bytes};
use lazy_static::lazy_static;

use crate::taiko::pacaya::BlockParams;

lazy_static! {
    static ref IS_PROPOSE_DELAYED: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
}

pub const BLOBS_SAFE_SIZE: usize = 125_000; // 131072 with some buffer
/// Use max 3 blobs, potentially we could exceed this but the buffer should be enough
pub const TARGET_BATCH_SIZE: usize = 3 * BLOBS_SAFE_SIZE;

pub fn set_propose_ok() {
    IS_PROPOSE_DELAYED.store(false, Ordering::Relaxed);
}
pub fn set_propose_delayed() {
    IS_PROPOSE_DELAYED.store(true, Ordering::Relaxed);
}
pub fn is_propose_delayed() -> bool {
    IS_PROPOSE_DELAYED.load(Ordering::Relaxed)
}

#[derive(Debug, Default, Clone)]
pub struct ProposeBatchParams {
    pub anchor_block_id: u64,
    pub start_block_num: u64,
    pub end_block_num: u64,
    pub last_timestamp: u64,
    pub block_params: Vec<BlockParams>,
    /// all txs in the blocks, without anchor tx
    pub all_tx_list: Vec<Arc<TxEnvelope>>,
    /// Encoded / compressed tx lists
    pub compressed: Bytes,
    pub coinbase: Address,
}

#[derive(Debug, Clone)]
pub enum ProposalRequest {
    Batch(ProposeBatchParams),
    Resync { origin: u64, end: u64 },
}

pub struct ProposerContext {
    /// The address of the proposer, which is set by the PreconfTaskManager if enabled; otherwise,
    /// it must be address(0)
    pub proposer: Address,
    /// The address that will receive the block rewards; defaults to the proposer's address if set
    /// to address(0).
    pub coinbase: Address,
}
