use std::{io::Write, sync::Arc};

use alloy_consensus::{BlobTransactionSidecar, TxEnvelope};
use alloy_eips::eip4844::MAX_BLOBS_PER_BLOCK;
use alloy_primitives::{Address, Bytes, B256};
use alloy_sol_types::{SolCall, SolValue};
use libflate::zlib::Encoder as zlibEncoder;
use tracing::debug;

use super::{preconf::PreconfRouter, BatchParams};
use crate::{
    proposer::ProposeBatchParams,
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
) -> Bytes {
    let compressed = request.compressed;

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

    PreconfRouter::proposeBatchCall { _params: encoded_params.into(), _txList: compressed }
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
) -> (Bytes, BlobTransactionSidecar) {
    let compressed = request.compressed;

    assert!(
        compressed.len() / MAX_BLOB_DATA_SIZE <= MAX_BLOBS_PER_BLOCK,
        "too many bytes to encode in blobs {}",
        compressed.len()
    );

    let blobs = compressed.chunks(MAX_BLOB_DATA_SIZE).map(encode_blob).collect();
    let sidecar = blobs_to_sidecar(blobs);

    debug!(
        blobs = sidecar.blobs.len(),
        proof_size = sidecar.proofs.len(),
        commitments = sidecar.commitments.len(),
        "blob sidecar"
    );

    let blob_params = BlobParams {
        blobHashes: vec![],
        firstBlobIndex: 0,
        numBlobs: sidecar.blobs.len() as u8,
        byteOffset: 0,
        byteSize: compressed.len() as u32,
    };

    debug!(?blob_params, "blob params");

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

    let input =
        PreconfRouter::proposeBatchCall { _params: encoded_params.into(), _txList: Bytes::new() }
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

pub fn encode_and_compress_tx_list(tx_list: Vec<Arc<TxEnvelope>>) -> Bytes {
    let encoded = alloy_rlp::encode(tx_list);
    compress_bytes(&encoded).into()
}
