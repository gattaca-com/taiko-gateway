use std::{
    io::{Read, Write},
    sync::Arc,
    time::Instant,
};

use alloy_consensus::{BlobTransactionSidecar, TxEnvelope};
use alloy_eips::eip4844::MAX_BLOBS_PER_BLOCK;
use alloy_primitives::{aliases::U96, Address, Bytes, B256};
use alloy_sol_types::{SolCall, SolValue};
use libflate::zlib::{Decoder as zlibDecoder, Encoder as zlibEncoder};
use tracing::debug;

use super::{
    preconf::PreconfRouter::{self, proposeBatchWithExpectedLastBlockIdCall},
    BatchParams,
};
use crate::{
    proposer::{skip_expected_block, ProposeBatchParams},
    sequencer::Order,
    taiko::{
        blob::{blobs_to_sidecar, encode_blob, MAX_BLOB_DATA_SIZE},
        pacaya::{BlobParams, BlockParams},
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

    if skip_expected_block() {
        PreconfRouter::proposeBatchCall { _params: encoded_params.into(), _txList: Bytes::new() }
            .abi_encode()
            .into()
    } else {
        PreconfRouter::proposeBatchWithExpectedLastBlockIdCall {
            _params: encoded_params.into(),
            _txList: Bytes::new(),
            _expectedLastBlockId: U96::from(request.end_block_num),
        }
        .abi_encode()
        .into()
    }
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

    let forced_batch_params = request
        .forced
        .map(|f| {
            // https://github.com/taikoxyz/taiko-mono/blob/f78dc19208366a7cb3d2b05b604263ad3c4db225/packages/taiko-client/proposer/transaction_builder/common.go#L28
            let params = BatchParams {
                proposer,
                coinbase: request.coinbase,
                parentMetaHash: parent_meta_hash,
                anchorBlockId: request.anchor_block_id,
                lastBlockTimestamp: f.timestamp,
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

    let input = if skip_expected_block() {
        PreconfRouter::proposeBatchCall { _params: encoded_params.into(), _txList: Bytes::new() }
            .abi_encode()
            .into()
    } else {
        PreconfRouter::proposeBatchWithExpectedLastBlockIdCall {
            _params: encoded_params.into(),
            _txList: Bytes::new(),
            _expectedLastBlockId: U96::from(request.end_block_num),
        }
        .abi_encode()
        .into()
    };

    (input, sidecar)
}

pub fn decode_propose_batch_with_expected_last_block_id_call(
    data: &[u8],
) -> eyre::Result<(u64, u64, u64)> {
    let call = proposeBatchWithExpectedLastBlockIdCall::abi_decode(data, true)?;
    let (_, params) = <(Bytes, Bytes)>::abi_decode_params(&call._params, true)?;
    let batch_params = BatchParams::abi_decode(&params, true)?;

    let n_blocks = batch_params.blocks.len();
    let last_block: u64 = call._expectedLastBlockId.try_into().unwrap();
    let start_block = last_block - n_blocks as u64 + 1;

    Ok((start_block, last_block, batch_params.anchorBlockId))
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
    let expected_len = estimate_compressed_size(encoded_len);

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

pub fn estimate_compressed_size(uncompressed_size: usize) -> usize {
    let uncompressed_size_f64 = uncompressed_size as f64;
    let slope = 0.9881615963738659;
    let intercept = -0.19936358812829091;
    let predicted_size = (slope * uncompressed_size_f64.ln() + intercept).exp();
    predicted_size as usize
}

fn decompress_bytes(data: &[u8]) -> Vec<u8> {
    let mut decoder = zlibDecoder::new(data).unwrap();
    let mut buffer = Vec::new();
    decoder.read_to_end(&mut buffer).unwrap();
    buffer
}

pub fn decode_tx_list(tx_list: &[u8]) -> eyre::Result<Vec<TxEnvelope>> {
    let decompressed = decompress_bytes(tx_list);
    let txs = alloy_rlp::decode_exact(decompressed)?;
    Ok(txs)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::taiko::pacaya::BlockParams;

    #[test]
    fn test_encode_decode() {
        let request = ProposeBatchParams {
            anchor_block_id: 99,
            coinbase: Address::ZERO,
            last_timestamp: 1,
            start_block_num: 1,
            end_block_num: 2,
            block_params: vec![
                BlockParams { numTransactions: 10, timeShift: 1, signalSlots: vec![] },
                BlockParams { numTransactions: 10, timeShift: 1, signalSlots: vec![] },
            ],
            all_tx_list: vec![],
            compressed_est: 0,
            forced: None,
        };

        let data = propose_batch_calldata(request, B256::ZERO, Address::ZERO, Bytes::new());
        let (start_block, last_block, anchor_block_id) =
            decode_propose_batch_with_expected_last_block_id_call(&data).unwrap();
        assert_eq!(start_block, 1);
        assert_eq!(last_block, 2);
        assert_eq!(anchor_block_id, 99);
    }
}
