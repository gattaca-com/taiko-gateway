use alloy_consensus::BlobTransactionSidecar;
use alloy_eips::eip4844::env_settings::EnvKzgSettings;
use c_kzg::{KzgCommitment, KzgProof, BYTES_PER_BLOB};
use eyre::bail;

// panic if this fails
pub fn blobs_to_sidecar(blobs: Vec<c_kzg::Blob>) -> BlobTransactionSidecar {
    let kzg_settings = EnvKzgSettings::Default.get();

    let mut commitments = Vec::with_capacity(blobs.len());
    let mut proofs = Vec::with_capacity(blobs.len());

    for blob in blobs.iter() {
        let commitment = KzgCommitment::blob_to_kzg_commitment(blob, kzg_settings)
            .expect("failed to compute commitment")
            .to_bytes();
        let proof = KzgProof::compute_blob_kzg_proof(blob, &commitment, kzg_settings)
            .expect("failed to compute proof")
            .to_bytes();
        commitments.push(commitment);
        proofs.push(proof);
    }

    BlobTransactionSidecar::from_kzg(blobs, commitments, proofs)
}

/// https://github.com/ethereum-optimism/optimism/blob/fbf69c9bd633cb247def3ca96b153c02baeb103e/op-service/eth/blob.go
pub const MAX_BLOB_DATA_SIZE: usize = (4 * 31 + 3) * 1024 - 4;
const ROUNDS: usize = 1024;
const ENCODING_VERSION: u8 = 0;
type Blob = [u8; BYTES_PER_BLOB];

/// Encodes the given input data into this blob. The encoding scheme is as follows:
///
/// In each round we perform 7 reads of input of lengths (31,1,31,1,31,1,31) bytes respectively for
/// a total of 127 bytes. This data is encoded into the next 4 field elements of the output by
/// placing each of the 4x31 byte chunks into bytes [1:32] of its respective field element. The
/// three single byte chunks (24 bits) are split into 4x6-bit chunks, each of which is written into
/// the top most byte of its respective field element, leaving the top 2 bits of each field element
/// empty to avoid modulus overflow.  This process is repeated for up to 1024 rounds until all data
/// is encoded.
///
/// For only the very first output field, bytes [1:5] are used to encode the version and the length
/// of the data.
pub fn encode_blob(data: &[u8]) -> c_kzg::Blob {
    assert!(data.len() <= MAX_BLOB_DATA_SIZE, "blob input too large");

    let mut blob = [0; BYTES_PER_BLOB];
    let mut read_offset = 0;
    let mut write_offset = 0;
    let mut buf31 = [0u8; 31];

    // Helper function to read 1 byte of input, or 0 if no input is left.
    fn read_1(data: &[u8], read_offset: &mut usize) -> u8 {
        if *read_offset >= data.len() {
            0
        } else {
            let out = data[*read_offset];
            *read_offset += 1;
            out
        }
    }

    // Helper function to read up to 31 bytes of input into buf31, left-aligned, padding with zeros.
    fn read_31(data: &[u8], read_offset: &mut usize, buf31: &mut [u8; 31]) {
        if *read_offset >= data.len() {
            buf31.fill(0);
        } else {
            let remaining = data.len() - *read_offset;
            let n = std::cmp::min(31, remaining);
            buf31[..n].copy_from_slice(&data[*read_offset..*read_offset + n]);
            buf31[n..].fill(0);
            *read_offset += n;
        }
    }

    // Helper function to write a single byte to the blob, ensuring correct offset and value.
    fn write_1(blob: &mut Blob, write_offset: &mut usize, v: u8) {
        assert!(
            *write_offset % 32 == 0,
            "blob encoding: invalid byte write offset: {}",
            *write_offset
        );
        assert!(v & 0b1100_0000 == 0, "blob encoding: invalid 6-bit value: 0b{:08b}", v);
        blob[*write_offset] = v;
        *write_offset += 1;
    }

    // Helper function to write buf31 to the blob, ensuring correct offset.
    fn write_31(blob: &mut Blob, write_offset: &mut usize, buf31: &[u8; 31]) {
        assert!(
            *write_offset % 32 == 1,
            "blob encoding: invalid bytes31 write offset: {}",
            *write_offset
        );
        blob[*write_offset..*write_offset + 31].copy_from_slice(buf31);
        *write_offset += 31;
    }

    for round in 0..ROUNDS {
        if read_offset >= data.len() {
            break;
        }

        // The first field element encodes the version and the length of the data in [1:5].
        if round == 0 {
            buf31[0] = ENCODING_VERSION;
            // Encode the length as big-endian uint24.
            let ilen = data.len() as u32;
            buf31[1] = (ilen >> 16) as u8;
            buf31[2] = (ilen >> 8) as u8;
            buf31[3] = ilen as u8;

            // Copy up to 27 bytes of data into buf31[4..], padding with zeros if necessary.
            let remaining = data.len() - read_offset;
            let n = std::cmp::min(27, remaining); // buf31[4:] can hold 27 bytes
            buf31[4..4 + n].copy_from_slice(&data[read_offset..read_offset + n]);
            buf31[4 + n..].fill(0);
            read_offset += n;
        } else {
            read_31(data, &mut read_offset, &mut buf31);
        }

        let x = read_1(data, &mut read_offset);
        let a = x & 0b0011_1111;
        write_1(&mut blob, &mut write_offset, a);
        write_31(&mut blob, &mut write_offset, &buf31);

        read_31(data, &mut read_offset, &mut buf31);
        let y = read_1(data, &mut read_offset);
        let b = (y & 0b0000_1111) | ((x & 0b1100_0000) >> 2);
        write_1(&mut blob, &mut write_offset, b);
        write_31(&mut blob, &mut write_offset, &buf31);

        read_31(data, &mut read_offset, &mut buf31);
        let z = read_1(data, &mut read_offset);
        let c = z & 0b0011_1111;
        write_1(&mut blob, &mut write_offset, c);
        write_31(&mut blob, &mut write_offset, &buf31);

        read_31(data, &mut read_offset, &mut buf31);
        let d = ((z & 0b1100_0000) >> 2) | ((y & 0b1111_0000) >> 4);
        write_1(&mut blob, &mut write_offset, d);
        write_31(&mut blob, &mut write_offset, &buf31);
    }

    assert!(
        read_offset == data.len(),
        "expected to fit data but failed {} {}",
        read_offset,
        data.len()
    );

    c_kzg::Blob::from(blob)
}

const BLOB_FIELD_ELEMENT_NUM: usize = 4096;
const BLOB_FIELD_ELEMENT_BYTES: usize = 32;
const BLOB_DATA_CAPACITY: usize = BLOB_FIELD_ELEMENT_NUM * BLOB_FIELD_ELEMENT_BYTES;
const BLOB_VERSION_OFFSET: usize = 1;

// https://github.com/taikoxyz/raiko/blob/f00b4051cf2a7e0dd1ec74e24b991ef7bd372d1c/lib/src/utils.rs#L193
pub fn decode_blob_data(blob_buf: &[u8]) -> Vec<u8> {
    // check the version
    if blob_buf[BLOB_VERSION_OFFSET] != ENCODING_VERSION {
        return Vec::new();
    }

    // decode the 3-byte big-endian length value into a 4-byte integer
    let output_len = (u32::from(blob_buf[2]) << 16 |
        u32::from(blob_buf[3]) << 8 |
        u32::from(blob_buf[4])) as usize;

    if output_len > MAX_BLOB_DATA_SIZE {
        return Vec::new();
    }

    // round 0 is special cased to copy only the remaining 27 bytes of the first field element
    // into the output due to version/length encoding already occupying its first 5 bytes.
    let mut output = [0; MAX_BLOB_DATA_SIZE];
    output[0..27].copy_from_slice(&blob_buf[5..32]);

    // now process remaining 3 field elements to complete round 0
    let mut opos: usize = 28; // current position into output buffer
    let mut ipos: usize = 32; // current position into the input blob
    let mut encoded_byte: [u8; 4] = [0; 4]; // buffer for the 4 6-bit chunks
    encoded_byte[0] = blob_buf[0];
    for encoded_byte_i in encoded_byte.iter_mut().skip(1) {
        let Ok(res) = decode_field_element(blob_buf, opos, ipos, &mut output) else {
            return Vec::new();
        };

        (*encoded_byte_i, opos, ipos) = res;
    }
    opos = reassemble_bytes(opos, encoded_byte, &mut output);

    // in each remaining round we decode 4 field elements (128 bytes) of the input into 127
    // bytes of output
    for _ in 1..1024 {
        if opos < output_len {
            for encoded_byte_j in &mut encoded_byte {
                // save the first byte of each field element for later re-assembly
                let Ok(res) = decode_field_element(blob_buf, opos, ipos, &mut output) else {
                    return Vec::new();
                };

                (*encoded_byte_j, opos, ipos) = res;
            }
            opos = reassemble_bytes(opos, encoded_byte, &mut output);
        }
    }
    for otailing in output.iter().skip(output_len) {
        if *otailing != 0 {
            return Vec::new();
        }
    }
    for itailing in blob_buf.iter().take(BLOB_DATA_CAPACITY).skip(ipos) {
        if *itailing != 0 {
            return Vec::new();
        }
    }
    output[0..output_len].to_vec()
}

fn decode_field_element(
    b: &[u8],
    opos: usize,
    ipos: usize,
    output: &mut [u8],
) -> eyre::Result<(u8, usize, usize)> {
    // two highest order bits of the first byte of each field element should always be 0
    if b[ipos] & 0b1100_0000 != 0 {
        bail!("ErrBlobInvalidFieldElement: field element: {ipos}");
    }
    // copy(output[opos:], b[ipos+1:ipos+32])
    output[opos..opos + 31].copy_from_slice(&b[ipos + 1..ipos + 32]);
    Ok((b[ipos], opos + 32, ipos + 32))
}

fn reassemble_bytes(
    opos: usize,
    encoded_byte: [u8; 4],
    output: &mut [u8; MAX_BLOB_DATA_SIZE],
) -> usize {
    // account for fact that we don't output a 128th byte
    let opos = opos - 1;
    let x = (encoded_byte[0] & 0b0011_1111) | ((encoded_byte[1] & 0b0011_0000) << 2);
    let y = (encoded_byte[1] & 0b0000_1111) | ((encoded_byte[3] & 0b0000_1111) << 4);
    let z = (encoded_byte[2] & 0b0011_1111) | ((encoded_byte[3] & 0b0011_0000) << 2);
    // put the re-assembled bytes in their appropriate output locations
    output[opos - 32] = z;
    output[opos - (32 * 2)] = y;
    output[opos - (32 * 3)] = x;
    opos
}
