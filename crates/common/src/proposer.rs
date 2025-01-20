use std::sync::Arc;

use alloy_primitives::{Address, B256};
use alloy_rpc_types::Block;

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
    /// Arbitary graffiti
    pub anchor_input: B256,
}
