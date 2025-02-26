use std::{
    fmt::Display,
    ops::{Add, Deref},
    sync::Arc,
};

use alloy_consensus::TxEnvelope;
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::{Address, Bytes};
use alloy_rlp::Decodable;
use alloy_rpc_types::Block;
use serde::{Deserialize, Serialize};

/// Request to sequence a transaction
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Order {
    /// Recoverred signer
    sender: Address,
    tx: TxEnvelope,
    /// Raw RLP encoded transaction
    raw: Bytes,
}

impl Order {
    pub fn decode(raw: Bytes) -> eyre::Result<Self> {
        let tx = TxEnvelope::decode(&mut raw.as_ref())?;
        Self::new(tx)
    }

    pub fn new_with_sender(tx: TxEnvelope, sender: Address) -> Self {
        let raw = tx.encoded_2718().into();
        Self { sender, tx, raw }
    }

    pub fn new(tx: TxEnvelope) -> eyre::Result<Self> {
        let raw = tx.encoded_2718().into();
        let sender = tx.recover_signer()?;
        Ok(Self { sender, tx, raw })
    }

    pub fn raw(&self) -> &Bytes {
        &self.raw
    }

    pub fn tx(&self) -> &TxEnvelope {
        &self.tx
    }

    pub fn sender(&self) -> &Address {
        &self.sender
    }
}

impl Deref for Order {
    type Target = TxEnvelope;

    fn deref(&self) -> &Self::Target {
        &self.tx
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
    pub const fn new(id: u64) -> Self {
        Self(id)
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
pub struct SealBlockResponse {
    /// Cumulative builder payment in the block
    #[serde(with = "alloy_serde::quantity")]
    pub cumulative_builder_payment: u128,
    pub cumulative_gas_used: u64,
    /// Finalised block
    pub built_block: Arc<Block>,
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
