use std::sync::Arc;

use alloy_consensus::TxEnvelope;
use alloy_primitives::{Address, Bytes, B256};
use alloy_rlp::RlpEncodable;
use alloy_rpc_types::{Block, Header};
use jsonrpsee::core::Serialize;
use pc_common::taiko::pacaya::encode_and_compress_tx_list;
use serde::Deserialize;
use tracing::debug;

#[derive(Debug, Default, Serialize, RlpEncodable)]
#[serde(rename_all = "camelCase")]
pub struct ExecutableData {
    parent_hash: B256,
    fee_recipient: Address,
    block_number: u64,
    gas_limit: u64,
    timestamp: u64,
    transactions: Bytes,
    extra_data: Bytes,
    base_fee_per_gas: u64,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildPreconfBlockRequestBody {
    executable_data: ExecutableData,
    end_of_sequencing: bool,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildPreconfBlockResponseBody {
    block_header: Header,
}

impl BuildPreconfBlockRequestBody {
    pub fn new(block: &Block, end_of_sequencing: bool) -> Self {
        let tx_list: Vec<Arc<TxEnvelope>> =
            block.transactions.txns().map(|tx| tx.inner.clone().into_inner().into()).collect();

        debug!(n_txs = tx_list.len(), "creating soft block");

        let compressed = encode_and_compress_tx_list(tx_list, true);

        let executable_data = ExecutableData {
            parent_hash: block.header.parent_hash,
            fee_recipient: block.header.beneficiary,
            block_number: block.header.number,
            gas_limit: block.header.gas_limit,
            timestamp: block.header.timestamp,
            transactions: compressed,
            extra_data: block.header.extra_data.clone(),
            base_fee_per_gas: block.header.base_fee_per_gas.unwrap(),
        };
        BuildPreconfBlockRequestBody { executable_data, end_of_sequencing }
    }
}

#[derive(Debug, Deserialize)]
pub struct StatusResponse {
    #[serde(rename = "highestUnsafeL2PayloadBlockID")]
    pub highest_unsafe_l2_payload_block_id: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_build_preconf_block_request_body() {
        let raw = r#"{
            "lookahead": {
                "currOperator": "0x591317b806b96262c07105cc06cb4831008afdf2",
                "nextOperator": "0x591317b806b96262c07105cc06cb4831008afdf2",
                "currRanges": null,
                "nextRanges": null,
                "updatedAt": "2025-05-14T20:39:12.089999015Z"
            },
            "totalCached": 0,
            "highestUnsafeL2PayloadBlockID": 306988,
            "endOfSequencingBlockHash": "0x0000000000000000000000000000000000000000000000000000000000000000"
        }"#;

        let deser = serde_json::from_str::<StatusResponse>(raw).unwrap();
        assert_eq!(deser.highest_unsafe_l2_payload_block_id, 306988);
    }
}
