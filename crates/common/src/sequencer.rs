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
    tx: Arc<TxEnvelope>,
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
        Self { sender, tx: Arc::new(tx), raw }
    }

    pub fn new(tx: TxEnvelope) -> eyre::Result<Self> {
        let raw = tx.encoded_2718().into();
        let sender = tx.recover_signer()?;
        Ok(Self { sender, tx: Arc::new(tx), raw })
    }

    pub fn raw(&self) -> &Bytes {
        &self.raw
    }

    pub fn tx(&self) -> Arc<TxEnvelope> {
        self.tx.clone()
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

#[derive(Debug, Clone, Deserialize)]
pub struct SimulateTxResponse {
    pub execution_result: ExecutionResult,
}

impl SimulateTxResponse {
    pub fn is_success(&self) -> bool {
        matches!(self.execution_result, ExecutionResult::Success { .. })
    }
}

#[derive(Debug, Clone, Deserialize)]
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
    Invalid { reason: InvalidReason },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InvalidReason {
    NonceTooLow { tx: u64, state: u64 },
    NonceTooHigh { tx: u64, state: u64 },
    NotEnoughGas,
    CapLessThanBaseFee,
    InsufficientFunds { have: u128, want: u128 },
    StateIdNotFound { state: u64 },
    Uknown(String),
}

impl<'de> Deserialize<'de> for InvalidReason {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let s = String::deserialize(deserializer)?;

        if let Some(captures) = s.strip_prefix("nonce too low: address ") {
            if let Some(rest) = captures.split_once(", tx: ") {
                if let Some((tx_str, state_str)) = rest.1.split_once(" state: ") {
                    if let (Ok(tx), Ok(state)) = (tx_str.parse::<u64>(), state_str.parse::<u64>()) {
                        return Ok(InvalidReason::NonceTooLow { tx, state });
                    }
                }
            }
        }

        if let Some(captures) = s.strip_prefix("nonce too high: address ") {
            if let Some(rest) = captures.split_once(", tx: ") {
                if let Some((tx_str, state_str)) = rest.1.split_once(" state: ") {
                    if let (Ok(tx), Ok(state)) = (tx_str.parse::<u64>(), state_str.parse::<u64>()) {
                        return Ok(InvalidReason::NonceTooHigh { tx, state });
                    }
                }
            }
        }

        if s.contains("not enough gas for further transactions") {
            return Ok(InvalidReason::NotEnoughGas);
        }

        if s.contains("max fee per gas less than block base fee") {
            return Ok(InvalidReason::CapLessThanBaseFee);
        }

        if let Some(captures) = s.strip_prefix("state not found for id ") {
            if let Ok(state) = captures.parse::<u64>() {
                return Ok(InvalidReason::StateIdNotFound { state });
            }
        }

        if let Some(captures) =
            s.strip_prefix("insufficient funds for gas * price + value: address ")
        {
            if let Some(rest) = captures.split_once(" have ") {
                if let Some((have, want)) = rest.1.split_once(" want ") {
                    if let (Ok(have), Ok(want)) = (have.parse::<u128>(), want.parse::<u128>()) {
                        return Ok(InvalidReason::InsufficientFunds { have, want });
                    }
                }
            }
        }

        Ok(InvalidReason::Uknown(s))
    }
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

        match decoded.execution_result {
            ExecutionResult::Success { state_id, gas_used, builder_payment } => {
                assert_eq!(state_id, StateId(128181565));
                assert_eq!(gas_used, 21000);
                assert_eq!(builder_payment, 20480);
            }
            _ => panic!("expected success"),
        }
    }

    #[test]
    fn test_invalid_too_low_reason_serde() {
        let data = r#"{"execution_result": {"invalid": {"reason": "nonce too low: address 0x1234567890abcdef, tx: 1 state: 100"}}}"#;
        let decoded: SimulateTxResponse = serde_json::from_str(&data).unwrap();

        match decoded.execution_result {
            ExecutionResult::Invalid { reason } => {
                assert_eq!(reason, InvalidReason::NonceTooLow { tx: 1, state: 100 });
            }
            _ => panic!("expected invalid"),
        }
    }

    #[test]
    fn test_invalid_too_high_reason_serde() {
        let data = r#"{"execution_result": {"invalid": {"reason": "nonce too high: address 0x1234567890abcdef, tx: 100 state: 1"}}}"#;
        let decoded: SimulateTxResponse = serde_json::from_str(&data).unwrap();

        match decoded.execution_result {
            ExecutionResult::Invalid { reason } => {
                assert_eq!(reason, InvalidReason::NonceTooHigh { tx: 100, state: 1 });
            }
            _ => panic!("expected invalid"),
        }
    }

    #[test]
    fn test_invalid_insufficient_funds_reason_serde() {
        let data = r#"{"execution_result": {"invalid": {"reason": "insufficient funds for gas * price + value: address 0xD6EE8EfBC7F9e0D6c5d182Da5841DbC4Db6CD5Ed have 56831218464375 want 114297842054944"}}}"#;
        let decoded: SimulateTxResponse = serde_json::from_str(&data).unwrap();

        match decoded.execution_result {
            ExecutionResult::Invalid { reason } => {
                assert_eq!(reason, InvalidReason::InsufficientFunds {
                    have: 56831218464375,
                    want: 114297842054944
                });
            }
            _ => panic!("expected invalid"),
        }
    }

    #[test]
    fn test_invalid_state_id_not_found_reason_serde() {
        let data = r#"{"execution_result": {"invalid": {"reason": "state not found for id 228"}}}"#;
        let decoded: SimulateTxResponse = serde_json::from_str(&data).unwrap();

        match decoded.execution_result {
            ExecutionResult::Invalid { reason } => {
                assert_eq!(reason, InvalidReason::StateIdNotFound { state: 228 });
            }
            _ => panic!("expected invalid"),
        }
    }
}
