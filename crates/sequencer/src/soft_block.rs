use std::sync::Arc;

use alloy_consensus::TxEnvelope;
use alloy_primitives::{Address, Bytes, PrimitiveSignature, B256};
use alloy_rlp::RlpEncodable;
use alloy_rpc_types::{Block, Header};
use jsonrpsee::core::Serialize;
use pc_common::taiko::pacaya::encode_and_compress_tx_list;
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
    base_fee_per_gas: u64,
    extra_data: Bytes,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct BuildPreconfBlockRequestBody {
    executable_data: ExecutableData,
    signature: String,
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
            gas_limit: block.header.gas_limit,
            timestamp: block.header.timestamp,
            transactions: compressed.into(),
            extra_data: block.header.extra_data.clone(),
            base_fee_per_gas: block.header.base_fee_per_gas.unwrap(),
        };

        BuildPreconfBlockRequestBody {
            executable_data,
            signature: alloy_primitives::hex::encode_prefixed(
                PrimitiveSignature::test_signature().as_bytes(),
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use alloy_primitives::PrimitiveSignature;

    use super::{BuildPreconfBlockRequestBody, ExecutableData};

    #[test]
    fn test_encode() {
        let a = BuildPreconfBlockRequestBody {
            executable_data: ExecutableData::default(),
            signature: alloy_primitives::hex::encode_prefixed(
                PrimitiveSignature::test_signature().as_bytes(),
            ),
        };

        let a = serde_json::to_string(&a).unwrap();

        println!("{a}")
    }
}
