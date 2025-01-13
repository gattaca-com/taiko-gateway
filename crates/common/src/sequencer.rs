use std::{fmt::Display, ops::Add, sync::Arc, time::Instant};

use alloy_consensus::TxEnvelope;
use alloy_primitives::B256;
use alloy_rpc_types::Block;
use serde::{Deserialize, Serialize};

/// Request to sequence a transaction
// TODO: add chain_id once we have L1 too
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Order {
    Tx(Arc<TxEnvelope>),
}

impl Order {
    pub fn hash(&self) -> B256 {
        match self {
            Order::Tx(tx) => *tx.tx_hash(),
        }
    }
}

/// State identifier, used to uniquely identify a sequenced list of transactions
/// States ids 0-99 are reserved for internal use
#[derive(Serialize, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(transparent)]
pub struct StateId(u64);

impl Display for StateId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl std::fmt::Debug for StateId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl StateId {
    /// Reserved ID for committed head state
    pub const CHAIN_HEAD: Self = Self(1);

    /// Reserved ID for committed preconf state
    pub const PRECONF_HEAD: Self = Self(2);

    pub const fn new(id: u64) -> Self {
        Self(id)
    }

    pub fn is_chain_head(&self) -> bool {
        *self == Self::CHAIN_HEAD
    }

    pub fn is_preconf_head(&self) -> bool {
        *self == Self::PRECONF_HEAD
    }
}

impl Add<u64> for StateId {
    type Output = Self;

    fn add(self, rhs: u64) -> Self {
        Self(self.0 + rhs)
    }
}

impl From<u64> for StateId {
    fn from(id: u64) -> StateId {
        StateId(id)
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct SimulateTxResponse {
    pub execution_result: ExecutionResult,
}

impl SimulateTxResponse {
    pub fn is_success(&self) -> bool {
        matches!(self.execution_result, ExecutionResult::Success { .. })
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
// we could use ExecutionResult from revm but need to check if easy to return in geth
#[serde(rename_all = "lowercase")]
pub enum ExecutionResult {
    /// Transaction was successful
    Success {
        state_id: StateId,
        #[serde(with = "alloy_serde::quantity")]
        gas_used: u128,
        #[serde(with = "alloy_serde::quantity")]
        builder_payment: u128,
    },
    /// Transaction reverted
    Revert {
        state_id: StateId,
        #[serde(with = "alloy_serde::quantity")]
        gas_used: u128,
        #[serde(with = "alloy_serde::quantity")]
        builder_payment: u128,
    },
    /// Transaction is invalid and can't be included in a block (e.g. wrong nonce)
    Invalid {
        // TODO: enum this
        reason: String,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommitStateResponse {
    /// Cumulative gas used in the block
    #[serde(with = "alloy_serde::quantity")]
    pub cumulative_gas_used: u128,
    /// Cumulative builder payment in the block
    #[serde(with = "alloy_serde::quantity")]
    pub cumulative_builder_payment: u128,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SealBlockResponse {
    /// Cumulative builder payment in the block
    #[serde(with = "alloy_serde::quantity")]
    pub cumulative_builder_payment: u128,
    /// Finalised block
    pub built_block: Arc<Block>,
}

pub enum SequenceLoop {
    // Start {
    //     /// Finalized block number to build on
    //     chain_head: u64,
    //     /// State root of the finalized block
    //     state_root: B256,
    // },
    Continue {
        /// When to stop sequencing, this acts as a "ping" from proposer module and makes sure
        /// we stop sequencing if the proposer is down
        stop_by: Instant,
    },
    /// Stop sequencing and finalize current block
    Stop,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn test_tx_result_serde() {
        let data = r#"{
            "execution_result": {
                "success": {
                    "state_id": 128181565,
                    "builder_payment": "0x5000",
                    "gas_used": "0x5208"
                }
            }
        }"#;
        let decoded: SimulateTxResponse = serde_json::from_str(&data).unwrap();

        assert_eq!(decoded.execution_result, ExecutionResult::Success {
            state_id: StateId(128181565),
            gas_used: 21000,
            builder_payment: 20480
        });
    }
}
