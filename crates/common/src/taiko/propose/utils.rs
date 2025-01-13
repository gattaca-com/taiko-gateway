use std::io::Write;

use alloy_consensus::TxEnvelope;
// use flate2::{bufread::ZlibEncoder, Compression};
use libflate::zlib::Encoder as zlibEncoder;

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

pub fn compress_bytes(data: &[u8]) -> Vec<u8> {
    let mut encoder = zlibEncoder::new(Vec::new()).unwrap();
    encoder.write_all(data).unwrap();
    encoder.finish().into_result().unwrap()
}

pub fn encode_and_compress_tx_list(tx_list: Vec<TxEnvelope>) -> Vec<u8> {
    let encoded = alloy_rlp::encode(tx_list);
    compress_bytes(&encoded)
}
