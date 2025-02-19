use std::sync::Arc;

use alloy_consensus::TxEnvelope;
use alloy_primitives::{Address, Bytes, PrimitiveSignature, B256};
use alloy_rlp::RlpEncodable;
use alloy_rpc_types::{Block, Header};
use jsonrpsee::core::Serialize;
use pc_common::taiko::{
    pacaya::encode_and_compress_tx_list, AnchorParams, BaseFeeConfig, ANCHOR_GAS_LIMIT,
};
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
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildPreconfBlockRequestBody {
    executable_data: ExecutableData,
    signature: String,
    #[serde(rename = "anchorBlockID")]
    anchor_block_id: u64,
    anchor_state_root: B256,
    signal_slots: Vec<[u8; 32]>,
    base_fee_config: BaseFeeConfig,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildPreconfBlockResponseBody {
    block_header: Header,
}

// FIXME
impl BuildPreconfBlockRequestBody {
    pub fn new(
        block: Arc<Block>,
        anchor_params: AnchorParams,
        base_fee_config: BaseFeeConfig,
    ) -> Self {
        // filter out anchor tx
        let tx_list: Vec<TxEnvelope> = block
            .transactions
            .txns()
            .map(|tx| tx.inner.clone())
            // .filter_map(|tx| {
            //     if tx.from == GOLDEN_TOUCH_ADDRESS {
            //         None
            //     } else {
            //         Some(tx.inner.clone())
            //     }
            // })
            .collect();

        debug!(n_txs = tx_list.len(), "creating soft block");

        let compressed = encode_and_compress_tx_list(tx_list);

        let executable_data = ExecutableData {
            parent_hash: block.header.parent_hash,
            fee_recipient: block.header.beneficiary,
            block_number: block.header.number,
            gas_limit: block.header.gas_limit - ANCHOR_GAS_LIMIT,
            timestamp: block.header.timestamp,
            transactions: compressed.into(),
        };

        BuildPreconfBlockRequestBody {
            executable_data,
            signature: alloy_primitives::hex::encode_prefixed(
                PrimitiveSignature::test_signature().as_bytes(),
            ),
            anchor_block_id: anchor_params.block_id,
            anchor_state_root: anchor_params.state_root,
            signal_slots: vec![],
            base_fee_config,
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{PrimitiveSignature, B256};
    use pc_common::taiko::BaseFeeConfig;

    use super::{BuildPreconfBlockRequestBody, ExecutableData};

    #[test]
    fn test_encode() {
        let a = BuildPreconfBlockRequestBody {
            executable_data: ExecutableData::default(),
            signature: alloy_primitives::hex::encode_prefixed(
                PrimitiveSignature::test_signature().as_bytes(),
            ),
            anchor_block_id: 0,
            anchor_state_root: B256::default(),
            signal_slots: vec![],
            base_fee_config: BaseFeeConfig {
                adjustment_quotient: 1,
                sharing_pctg: 1,
                gas_issuance_per_second: 1,
                min_gas_excess: 1,
                max_gas_issuance_per_block: 1,
            },
        };

        let a = serde_json::to_string(&a).unwrap();

        println!("{a}")
    }
}
