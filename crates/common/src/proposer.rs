use std::{fmt::{self, Display}, sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
}};

use alloy_primitives::Address;
use alloy_rpc_types::Block;
use lazy_static::lazy_static;

lazy_static! {
    static ref IS_PROPOSE_DELAYED: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
    static ref IS_RESYNCING: Arc<AtomicBool> = Arc::new(AtomicBool::new(false));
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

pub fn set_resyncing() {
    IS_RESYNCING.store(true, Ordering::Relaxed);
}

pub fn set_resync_complete() {
    IS_RESYNCING.store(false, Ordering::Relaxed);
}

pub fn is_resyncing() -> bool {
    IS_RESYNCING.load(Ordering::Relaxed)
}

#[derive(Debug, Clone)]
pub enum ProposerEvent {
    SealedBlock {
        block: Arc<Block>,
        anchor_block_id: u64,
    },
    NeedsResync,
}

impl Display for ProposerEvent {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProposerEvent::SealedBlock { block, anchor_block_id } => {
                write!(f, "SealedBlock {{ block_number: {}, anchor_block_id: {} }}", block.header.number, anchor_block_id)
            }
            ProposerEvent::NeedsResync => {
                write!(f, "NeedsResync")
            }
        }
    }
}

pub struct ProposerContext {
    /// The address of the proposer, which is set by the PreconfTaskManager if enabled; otherwise,
    /// it must be address(0)
    pub proposer: Address,
    /// The address that will receive the block rewards; defaults to the proposer's address if set
    /// to address(0).
    pub coinbase: Address,
}
