use alloy_consensus::TxEnvelope;
use alloy_primitives::{Bytes, B256};
use pc_common::{
    sequencer::{CommitStateResponse, Order, SealBlockResponse, SimulateTxResponse, StateId},
    types::{AnchorData, BlockEnv},
};

use crate::context::AnchorEnv;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkerRequest {
    Simulate { state_id: StateId, order: Order },
    Commit { state_id: StateId },
    Seal,
    Anchor { tx: TxEnvelope, anchor_data: AnchorData, block_env: Box<BlockEnv>, extra_data: Bytes },
}

/// Responses from simulator API
#[derive(Debug, Clone)]
pub enum WorkerResponse {
    Simulate {
        origin_state_id: StateId,
        new_state_id: StateId,
        gas_used: u128,
        /// Already checked > 0
        builder_payment: u128,
    },
    Anchor {
        env: AnchorEnv,
    },
    Commit {
        commit: CommitStateResponse,
        origin_state_id: StateId,
    },
    Block(SealBlockResponse),
}

#[derive(Debug, Clone, thiserror::Error)]
pub enum WorkerError {
    #[error("Connection error")]
    Connection,

    #[error("failed anchor tx simulation: {0:?}")]
    FailedAnchorSim(SimulateTxResponse),

    #[error("tx reverted: gas_used={0}")]
    Revert(u128),

    #[error("invalid tx: {0}")]
    InvalidOrHalt(String),

    #[error("zero payment tx: {0}")]
    ZeroPayment(B256),
}
