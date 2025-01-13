use alloy_consensus::BlobTransactionSidecar;
use alloy_primitives::{bytes, Address};
use alloy_rlp::encode;
use alloy_sol_types::{SolCall, SolValue};

use super::{
    blob::{blobs_to_sidecar, encode_blob},
    utils::compress_bytes,
};
use crate::{
    proposer::ProposeParams,
    taiko::{l1::TaikoL1, BlockParamsV2},
};

/// Txs should *not* include the anchor transaction
pub fn propose_block_v2_calldata(
    ProposeParams { anchor_block_id, anchor_timestamp, tx_list }: ProposeParams,
    coinbase: Address,
) -> Vec<u8> {
    // first get the RLP encoded tx list
    // ref: signedTransactionsToTxListBytes
    let encoded = alloy_rlp::encode(tx_list);

    // then encode the tx list using zlib
    let compressed = compress_bytes(&encoded);

    // no blob data
    let params = BlockParamsV2 {
        coinbase,
        anchorBlockId: anchor_block_id,
        timestamp: anchor_timestamp,
        ..Default::default()
    };
    let encoded_params = params.abi_encode_params();

    TaikoL1::proposeBlockV2Call { _params: encoded_params.into(), _txList: compressed.into() }
        .abi_encode()
}

/// Txs should *not* include the anchor transaction
pub fn propose_block_v2_blob(
    ProposeParams { anchor_block_id, anchor_timestamp, tx_list }: ProposeParams,
    coinbase: Address,
) -> eyre::Result<(Vec<u8>, BlobTransactionSidecar)> {
    // first get the RLP encoded tx list
    let tx_list = encode(tx_list);

    // then encode the tx list using zlib
    let compressed = compress_bytes(&tx_list);

    let blob = encode_blob(&compressed)?;

    let params = BlockParamsV2 {
        coinbase,
        anchorBlockId: anchor_block_id,
        timestamp: anchor_timestamp,
        blobIndex: 0,
        blobTxListOffset: 0,
        blobTxListLength: compressed.len() as u32,
        ..Default::default()
    };
    let encoded_params = params.abi_encode_params();

    let input = TaikoL1::proposeBlockV2Call { _params: encoded_params.into(), _txList: bytes!() }
        .abi_encode();
    let sidecar = blobs_to_sidecar(vec![blob])?;

    Ok((input, sidecar))
}

/// Txs should *not* include the anchor transaction
pub fn propose_blocks_v2_calldata(params: Vec<ProposeParams>, coinbase: Address) -> Vec<u8> {
    let mut params_arr = Vec::new();
    let mut tx_list_arr = Vec::new();

    for param in params {
        let encoded = alloy_rlp::encode(param.tx_list);
        let compressed = compress_bytes(&encoded);

        let block_params = BlockParamsV2 {
            coinbase,
            anchorBlockId: param.anchor_block_id,
            timestamp: param.anchor_timestamp,
            ..Default::default()
        };
        let encoded_params = block_params.abi_encode_params();

        params_arr.push(encoded_params.into());
        tx_list_arr.push(compressed.into());
    }

    TaikoL1::proposeBlocksV2Call { _paramsArr: params_arr, _txListArr: tx_list_arr }.abi_encode()
}

#[cfg(test)]
mod tests {
    use alloy_primitives::hex;
    use alloy_sol_types::SolValue;

    use super::*;
    use crate::taiko::BlockParamsV2;

    // this doesnt match Go for some reason
    #[test]
    fn compress_test() {
        let target = hex!("789c62646266616563e700040000ffff00800025");
        let tx_list: Vec<u8> = vec![1, 2, 3, 4, 5, 6, 7, 8];
        // let tx_list = alloy::rlp::encode(tx_list);

        let compressed = compress_bytes(&tx_list);
        println!("compressed {}", hex::encode(&compressed));

        assert_eq!(target.len(), compressed.len());
        assert_eq!(target, *compressed);
    }

    #[test]
    fn params_test() {
        let target = hex!("000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000007b00000000000000000000000000000000000000000000000000000000000011d7000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000");

        let anchor_block_id = 0123;
        let anchor_timestamp = 4567;
        let coinbase = Address::ZERO;

        let params = BlockParamsV2 {
            coinbase,
            anchorBlockId: anchor_block_id,
            timestamp: anchor_timestamp,
            ..Default::default()
        };

        let encoded_params = params.abi_encode();

        assert_eq!(target.len(), encoded_params.len());
        assert_eq!(target, *encoded_params);
    }
}
