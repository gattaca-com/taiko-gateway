use std::{io::Write, sync::Arc};

use alloy_consensus::BlobTransactionSidecar;
use alloy_eips::eip4844::MAX_BLOBS_PER_BLOCK;
use alloy_primitives::{Bytes, B256};
use alloy_rpc_types::Block;
use alloy_sol_types::{SolCall, SolValue};
use libflate::zlib::Encoder as zlibEncoder;

use super::{BatchParams, BlockParams};
use crate::{
    proposer::ProposerContext,
    taiko::{
        blob::{blobs_to_sidecar, encode_blob, MAX_BLOB_DATA_SIZE},
        pacaya::l1::TaikoL1,
        GOLDEN_TOUCH_ADDRESS,
    },
};

/// Returns the calldata for the proposeBatchCall
/// Blocks:
/// - need to share the same anchor_block_id
/// - are assumed to be sorted by block number and have all the same timestamp
pub fn propose_batch_calldata(
    full_blocks: Vec<Arc<Block>>,
    anchor_block_id: u64,
    parent_meta_hash: B256,
    context: &ProposerContext,
) -> Bytes {
    let mut blocks = Vec::with_capacity(full_blocks.len());
    let mut tx_list = Vec::new();
    let mut last_timestamp = 0;

    for block in full_blocks {
        let block = Arc::unwrap_or_clone(block);

        blocks.push(BlockParams { numTransactions: block.transactions.len() as u16, timeShift: 0 });

        let txs = block
            .transactions
            .into_transactions()
            .filter(|tx| tx.from != GOLDEN_TOUCH_ADDRESS)
            .map(|tx| tx.inner);

        tx_list.extend(txs);
        last_timestamp = block.header.timestamp;
    }

    let encoded = alloy_rlp::encode(tx_list);
    let compressed = compress_bytes(&encoded);

    // TODO: check the offsets here
    let batch_params = BatchParams {
        proposer: context.proposer,
        coinbase: context.coinbase,
        parentMetaHash: parent_meta_hash,
        anchorBlockId: anchor_block_id,
        anchorInput: context.anchor_input,
        lastBlockTimestamp: last_timestamp,
        txListOffset: 0,
        txListSize: compressed.len() as u32,
        firstBlobIndex: 0,
        numBlobs: 0,
        revertIfNotFirstProposal: false,
        signalSlots: vec![],
        blocks,
    };

    let encoded_params = batch_params.abi_encode_params();

    TaikoL1::proposeBatchCall { _params: encoded_params.into(), _txList: compressed.into() }
        .abi_encode()
        .into()
}

/// Returns the calldata and blob sidecar for the proposeBatchCall
/// Blocks:
/// - need to share the same anchor_block_id
/// - are assumed to be sorted by block number and have all the same timestamp
/// - are assumed to have less transactions in total that would fit in all available blobs
pub fn propose_batch_blobs(
    full_blocks: Vec<Arc<Block>>,
    anchor_block_id: u64,
    parent_meta_hash: B256,
    context: &ProposerContext,
) -> (Bytes, BlobTransactionSidecar) {
    let mut blocks = Vec::with_capacity(full_blocks.len());
    let mut tx_list = Vec::new();
    let mut last_timestamp = 0;

    for block in full_blocks {
        let block = Arc::unwrap_or_clone(block);

        blocks.push(BlockParams { numTransactions: block.transactions.len() as u16, timeShift: 0 });

        let txs = block
            .transactions
            .into_transactions()
            .filter(|tx| tx.from != GOLDEN_TOUCH_ADDRESS)
            .map(|tx| tx.inner);

        tx_list.extend(txs);
        last_timestamp = block.header.timestamp;
    }

    let encoded = alloy_rlp::encode(tx_list);
    let compressed = compress_bytes(&encoded);

    assert!(
        compressed.len() % MAX_BLOB_DATA_SIZE <= MAX_BLOBS_PER_BLOCK,
        "too many bytes to encode in blobs"
    );

    let blobs = compressed.chunks(MAX_BLOB_DATA_SIZE).map(encode_blob).collect();
    let sidecar = blobs_to_sidecar(blobs);

    let batch_params = BatchParams {
        proposer: context.proposer,
        coinbase: context.coinbase,
        parentMetaHash: parent_meta_hash,
        anchorBlockId: anchor_block_id,
        anchorInput: context.anchor_input,
        lastBlockTimestamp: last_timestamp,
        txListOffset: 0,
        txListSize: 0,
        firstBlobIndex: 0,
        numBlobs: sidecar.blobs.len() as u8,
        revertIfNotFirstProposal: false,
        signalSlots: vec![],
        blocks,
    };

    let encoded_params = batch_params.abi_encode_params();
    let input = TaikoL1::proposeBatchCall { _params: encoded_params.into(), _txList: Bytes::new() }
        .abi_encode()
        .into();

    (input, sidecar)
}

// /// ref: utils.Compress(txListBytes)
// /// raiko: utils::zlib_compress_data
// fn compress_bytes(bytes: &[u8]) -> Vec<u8> {
//     let mut buffer = Vec::new();
//     let mut e = ZlibEncoder::new(bytes, Compression::default());

//     e.read_to_end(&mut buffer).unwrap();

//     buffer
// }

// fn compress_bytes(bytes: &[u8]) -> Vec<u8> {
//     compress_to_vec_zlib(bytes, 6)
// }

fn compress_bytes(data: &[u8]) -> Vec<u8> {
    let mut encoder = zlibEncoder::new(Vec::new()).unwrap();
    encoder.write_all(data).unwrap();
    encoder.finish().into_result().unwrap()
}
