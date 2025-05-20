use std::{io::Write, sync::Arc, time::Instant};

use alloy_consensus::{BlobTransactionSidecar, TxEnvelope};
use alloy_eips::eip4844::MAX_BLOBS_PER_BLOCK;
use alloy_primitives::{aliases::U96, Address, Bytes, B256};
use alloy_sol_types::{SolCall, SolValue};
use libflate::zlib::Encoder as zlibEncoder;
use tracing::debug;

use super::{preconf::PreconfRouter, BatchParams};
use crate::{
    proposer::ProposeBatchParams,
    sequencer::Order,
    taiko::{
        blob::{blobs_to_sidecar, encode_blob, MAX_BLOB_DATA_SIZE},
        pacaya::BlobParams,
    },
};

/// Returns the calldata for the proposeBatchCall
/// Blocks:
/// - need to share the same anchor_block_id
/// - are assumed to be sorted by block number and have all the same timestamp
pub fn propose_batch_calldata(
    request: ProposeBatchParams,
    parent_meta_hash: B256,
    proposer: Address,
    compressed: Bytes,
) -> Bytes {
    // TODO: check the offsets here
    let batch_params = BatchParams {
        proposer,
        coinbase: request.coinbase,
        parentMetaHash: parent_meta_hash,
        anchorBlockId: request.anchor_block_id,
        lastBlockTimestamp: request.last_timestamp,
        revertIfNotFirstProposal: false,
        blocks: request.block_params,
        blobParams: BlobParams {
            blobHashes: vec![],
            firstBlobIndex: 0,
            numBlobs: 0,
            byteOffset: 0,
            byteSize: compressed.len() as u32,
        },
    };

    let forced_tx_list = Bytes::new();
    let encoded_params =
        (forced_tx_list, Bytes::from(batch_params.abi_encode())).abi_encode_params();

    PreconfRouter::proposeBatchWithExpectedLastBlockIdCall {
        _params: encoded_params.into(),
        _txList: compressed,
        _expectedLastBlockId: U96::from(request.end_block_num),
    }
    .abi_encode()
    .into()
}

/// Returns the calldata and blob sidecar for the proposeBatchCall
/// Blocks:
/// - need to share the same anchor_block_id
/// - are assumed to be sorted by block number and have all the same timestamp
/// - are assumed to have less transactions in total that would fit in all available blobs
pub fn propose_batch_blobs(
    request: ProposeBatchParams,
    parent_meta_hash: B256,
    proposer: Address,
    compressed: Bytes,
) -> (Bytes, BlobTransactionSidecar) {
    assert!(
        compressed.len() / MAX_BLOB_DATA_SIZE <= MAX_BLOBS_PER_BLOCK,
        "too many bytes to encode in blobs {}",
        compressed.len()
    );

    let blobs = compressed.chunks(MAX_BLOB_DATA_SIZE).map(encode_blob).collect();
    let sidecar = blobs_to_sidecar(blobs);

    let blob_params = BlobParams {
        blobHashes: vec![],
        firstBlobIndex: 0,
        numBlobs: sidecar.blobs.len() as u8,
        byteOffset: 0,
        byteSize: compressed.len() as u32,
    };

    let batch_params = BatchParams {
        proposer,
        coinbase: request.coinbase,
        parentMetaHash: parent_meta_hash,
        anchorBlockId: request.anchor_block_id,
        lastBlockTimestamp: request.last_timestamp,
        revertIfNotFirstProposal: false,
        blobParams: blob_params,
        blocks: request.block_params,
    };

    let forced_tx_list = Bytes::new();
    let encoded_params =
        (forced_tx_list, Bytes::from(batch_params.abi_encode())).abi_encode_params();

    let input = PreconfRouter::proposeBatchWithExpectedLastBlockIdCall {
        _params: encoded_params.into(),
        _txList: Bytes::new(),
        _expectedLastBlockId: U96::from(request.end_block_num),
    }
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

// TODO: re-use the RLP encoding here
pub fn encode_and_compress_orders(orders: Vec<Order>, log: bool) -> Bytes {
    let tx_list = orders.iter().map(|o| o.tx()).collect::<Vec<_>>();
    encode_and_compress_tx_list(tx_list, log)
}

pub fn encode_and_compress_tx_list(tx_list: Vec<Arc<TxEnvelope>>, log: bool) -> Bytes {
    let n_txs = tx_list.len();
    let start = Instant::now();
    let encoded = alloy_rlp::encode(tx_list);
    let encode_time = start.elapsed();
    let encoded_len = encoded.len();
    let start = Instant::now();
    let b = compress_bytes(&encoded);
    let compress_time = start.elapsed();
    let compress_len = b.len();

    let compressed_ratio = (compress_len as f64) / (encoded_len as f64);

    let expected_len = (COMPRESS_RATIO * encoded_len as f64).round() as usize;

    if log {
        debug!(
            n_txs,
            encoded_len,
            ?encode_time,
            ?compress_time,
            compress_len,
            expected_len,
            compressed_ratio,
            "compress tx list"
        );
    }

    b.into()
}

const COMPRESS_RATIO: f64 = 0.625;

pub fn estimate_compressed_size(uncompressed_size: usize) -> usize {
    (COMPRESS_RATIO * uncompressed_size as f64).round() as usize
}
