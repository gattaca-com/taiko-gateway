use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

use alloy_primitives::Address;
use alloy_rpc_types::Block;
use lazy_static::lazy_static;

lazy_static! {
    static ref IS_PROPOSE_DELAYED: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
}

pub fn set_propose_ok() {
    IS_PROPOSE_DELAYED.store(false, Ordering::Relaxed);
}
pub fn set_propose_delayed() {
    IS_PROPOSE_DELAYED.store(true, Ordering::Relaxed);
}
pub fn is_propose_delayed() -> bool {
    IS_PROPOSE_DELAYED.load(Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub struct NewSealedBlock {
    pub block: Arc<Block>,
    pub anchor_block_id: u64,
}

pub struct ProposerContext {
    /// The address of the proposer, which is set by the PreconfTaskManager if enabled; otherwise,
    /// it must be address(0)
    pub proposer: Address,
    /// The address that will receive the block rewards; defaults to the proposer's address if set
    /// to address(0).
    pub coinbase: Address,
}
