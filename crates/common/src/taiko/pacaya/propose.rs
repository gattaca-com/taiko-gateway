use std::{
    io::{Read, Write},
    sync::Arc,
};

use alloy_consensus::{BlobTransactionSidecar, TxEnvelope};
use alloy_eips::eip4844::MAX_BLOBS_PER_BLOCK;
use alloy_primitives::{Address, Bytes, B256};
use alloy_sol_types::{SolCall, SolValue};
use libflate::zlib::{Decoder as zlibDecoder, Encoder as zlibEncoder};

use super::{preconf::PreconfRouter, BatchParams, BlockParams};
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

    let forced_batch_params = request
        .forced
        .map(|f| {
            // https://github.com/taikoxyz/taiko-mono/blob/f78dc19208366a7cb3d2b05b604263ad3c4db225/packages/taiko-client/proposer/transaction_builder/common.go#L28
            let params = BatchParams {
                proposer,
                coinbase: request.coinbase,
                parentMetaHash: parent_meta_hash,
                anchorBlockId: request.anchor_block_id,
                lastBlockTimestamp: request.last_timestamp,
                revertIfNotFirstProposal: false,
                blobParams: BlobParams {
                    blobHashes: vec![f.inclusion.blobHash],
                    firstBlobIndex: 0,
                    numBlobs: 0,
                    byteOffset: f.inclusion.blobByteOffset,
                    byteSize: f.inclusion.blobByteSize,
                    createdIn: f.inclusion.blobCreatedIn,
                },
                blocks: vec![BlockParams {
                    numTransactions: f.min_txs_per_forced as u16,
                    timeShift: 0,
                    signalSlots: vec![],
                }],
            };
            Bytes::from(params.abi_encode())
        })
        .unwrap_or_default();

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
            createdIn: 0,
        },
    };

    let encoded_params =
        (forced_batch_params, Bytes::from(batch_params.abi_encode())).abi_encode_params();

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

    let forced_batch_params = request
        .forced
        .map(|f| {
            // https://github.com/taikoxyz/taiko-mono/blob/f78dc19208366a7cb3d2b05b604263ad3c4db225/packages/taiko-client/proposer/transaction_builder/common.go#L28
            let params = BatchParams {
                proposer,
                coinbase: request.coinbase,
                parentMetaHash: parent_meta_hash,
                anchorBlockId: request.anchor_block_id,
                lastBlockTimestamp: request.last_timestamp,
                revertIfNotFirstProposal: false,
                blobParams: BlobParams {
                    blobHashes: vec![f.inclusion.blobHash],
                    firstBlobIndex: 0,
                    numBlobs: 0,
                    byteOffset: f.inclusion.blobByteOffset,
                    byteSize: f.inclusion.blobByteSize,
                    createdIn: f.inclusion.blobCreatedIn,
                },
                blocks: vec![BlockParams {
                    numTransactions: f.min_txs_per_forced as u16,
                    timeShift: 0,
                    signalSlots: vec![],
                }],
            };
            Bytes::from(params.abi_encode())
        })
        .unwrap_or_default();

    let blobs = compressed.chunks(MAX_BLOB_DATA_SIZE).map(encode_blob).collect();
    let sidecar = blobs_to_sidecar(blobs);

    let blob_params = BlobParams {
        blobHashes: vec![],
        firstBlobIndex: 0,
        numBlobs: sidecar.blobs.len() as u8,
        byteOffset: 0,
        byteSize: compressed.len() as u32,
        createdIn: 0,
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

    let encoded_params =
        (forced_batch_params, Bytes::from(batch_params.abi_encode())).abi_encode_params();

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

fn decompress_bytes(data: &[u8]) -> Vec<u8> {
    let mut decoder = zlibDecoder::new(data).unwrap();
    let mut buffer = Vec::new();
    decoder.read_to_end(&mut buffer).unwrap();
    buffer
}

pub fn encode_and_compress_tx_list(tx_list: Vec<Arc<TxEnvelope>>) -> Bytes {
    let encoded = alloy_rlp::encode(tx_list);
    compress_bytes(&encoded).into()
}

pub fn decode_tx_list(tx_list: &[u8]) -> eyre::Result<Vec<TxEnvelope>> {
    let decompressed = decompress_bytes(tx_list);
    let txs = alloy_rlp::decode_exact(decompressed)?;
    Ok(txs)
}
