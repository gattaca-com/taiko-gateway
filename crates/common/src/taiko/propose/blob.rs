#![allow(dead_code)]

use alloy_consensus::BlobTransactionSidecar;
use alloy_eips::eip4844::env_settings::EnvKzgSettings;
use c_kzg::{KzgCommitment, KzgProof};
use eyre::bail;

pub(crate) fn blobs_to_sidecar(blobs: Vec<Blob>) -> eyre::Result<BlobTransactionSidecar> {
    let settings = EnvKzgSettings::Default.get();

    let blobs = blobs.into_iter().map(|blob| blob.into()).collect::<Vec<_>>();

    let mut commitments = Vec::with_capacity(blobs.len());
    let mut proofs = Vec::with_capacity(blobs.len());

    for blob in blobs.iter() {
        let commitment = KzgCommitment::blob_to_kzg_commitment(blob, settings)?;
        let proof = KzgProof::compute_blob_kzg_proof(blob, &commitment.to_bytes(), settings)?;
        commitments.push(commitment.to_bytes());
        proofs.push(proof.to_bytes());
    }

    // TODO: Fix those type conversions (same issue as in v1), if possible
    Ok(BlobTransactionSidecar::new(
        blobs.into_iter().map(|blob| blob.as_slice().try_into().unwrap()).collect(),
        commitments
            .into_iter()
            .map(|commitment| commitment.as_slice().try_into().unwrap())
            .collect(),
        proofs.into_iter().map(|proof| proof.as_slice().try_into().unwrap()).collect(),
    ))
}

/// https://github.com/ethereum-optimism/optimism/blob/fbf69c9bd633cb247def3ca96b153c02baeb103e/op-service/eth/blob.go#L88

pub const BLOB_SIZE: usize = 4096 * 32;
pub const MAX_BLOB_DATA_SIZE: usize = (4 * 31 + 3) * 1024 - 4;
const ROUNDS: usize = 1024;
const ENCODING_VERSION: u8 = 0;
// NOTE: Not used right now, but used for decoding - to know the version byte's index in the encoded
// blob
#[allow(unused)]
const VERSION_OFFSET: usize = 1;

pub type Blob = [u8; BLOB_SIZE];

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

#[allow(non_snake_case)]
pub fn encode_blob(data: &[u8]) -> eyre::Result<Blob> {
    if data.len() > MAX_BLOB_DATA_SIZE {
        bail!("blob input too large");
    }

    let mut blob = [0u8; BLOB_SIZE];
    let mut read_offset = 0;
    let mut write_offset = 0;
    let mut buf31 = [0u8; 31];

    // Helper function to read 1 byte of input, or 0 if no input is left.
    fn read1(data: &[u8], read_offset: &mut usize) -> u8 {
        if *read_offset >= data.len() {
            0
        } else {
            let out = data[*read_offset];
            *read_offset += 1;
            out
        }
    }

    // Helper function to read up to 31 bytes of input into buf31, left-aligned, padding with zeros.
    fn read31(data: &[u8], read_offset: &mut usize, buf31: &mut [u8; 31]) {
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
    fn write1(blob: &mut Blob, write_offset: &mut usize, v: u8) {
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
    fn write31(blob: &mut Blob, write_offset: &mut usize, buf31: &[u8; 31]) {
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
            read31(data, &mut read_offset, &mut buf31);
        }

        let x = read1(data, &mut read_offset);
        let A = x & 0b0011_1111;
        write1(&mut blob, &mut write_offset, A);
        write31(&mut blob, &mut write_offset, &buf31);

        read31(data, &mut read_offset, &mut buf31);
        let y = read1(data, &mut read_offset);
        let B = (y & 0b0000_1111) | ((x & 0b1100_0000) >> 2);
        write1(&mut blob, &mut write_offset, B);
        write31(&mut blob, &mut write_offset, &buf31);

        read31(data, &mut read_offset, &mut buf31);
        let z = read1(data, &mut read_offset);
        let C = z & 0b0011_1111;
        write1(&mut blob, &mut write_offset, C);
        write31(&mut blob, &mut write_offset, &buf31);

        read31(data, &mut read_offset, &mut buf31);
        let D = ((z & 0b1100_0000) >> 2) | ((y & 0b1111_0000) >> 4);
        write1(&mut blob, &mut write_offset, D);
        write31(&mut blob, &mut write_offset, &buf31);
    }

    if read_offset < data.len() {
        panic!(
            "Expected to fit data but failed, read offset: {}, data length: {}",
            read_offset,
            data.len()
        );
    }

    Ok(blob)
}

/// Decodes the given Blob into raw byte data.
/// Returns an error if the encoding is invalid.
#[allow(clippy::needless_range_loop)]
pub fn decode_blob(blob: &Blob) -> eyre::Result<Vec<u8>> {
    // Check the encoding version
    if blob[VERSION_OFFSET] != ENCODING_VERSION {
        bail!("Invalid encoding version");
    }

    // Decode the 3-byte big-endian length value into a usize
    let output_len = ((blob[2] as usize) << 16) | ((blob[3] as usize) << 8) | (blob[4] as usize);
    if output_len > MAX_BLOB_DATA_SIZE {
        bail!("Invalid length in blob");
    }

    // Initialize the output buffer and copy the first 27 bytes
    let mut output = vec![0u8; MAX_BLOB_DATA_SIZE];
    output[0..27].copy_from_slice(&blob[5..32]);

    // Initialize positions
    let mut opos = 28; // Output position
    let mut ipos = 32; // Input (blob) position

    let mut encoded_byte = [0u8; 4]; // Buffer for the 4 6-bit chunks
    encoded_byte[0] = blob[0]; // The first byte of the blob

    // Process remaining 3 field elements to complete round 0
    for i in 1..4 {
        let (byte, new_opos, new_ipos) = decode_field_element(blob, opos, ipos, &mut output)?;
        encoded_byte[i] = byte;
        opos = new_opos;
        ipos = new_ipos;
    }
    opos = reassemble_bytes(opos, &encoded_byte, &mut output)?;

    // Process remaining rounds
    let mut round = 1;
    while round < ROUNDS && opos < output_len {
        for j in 0..4 {
            // Decode the next field element
            let (byte, new_opos, new_ipos) = decode_field_element(blob, opos, ipos, &mut output)?;
            encoded_byte[j] = byte;
            opos = new_opos;
            ipos = new_ipos;
        }
        opos = reassemble_bytes(opos, &encoded_byte, &mut output)?;
        round += 1;
    }

    // Verify that the rest of the output buffer is zero
    for i in output_len..output.len() {
        if output[i] != 0 {
            bail!("Extraneous data in output buffer");
        }
    }

    // Trim the output to the actual length
    output.truncate(output_len);

    // Verify that the rest of the blob is zero
    for i in ipos..BLOB_SIZE {
        if blob[i] != 0 {
            bail!("Extraneous data in blob");
        }
    }

    Ok(output)
}

/// Decodes the next field element from the blob.
/// Returns the first byte of the field element and updates positions.
fn decode_field_element(
    blob: &Blob,
    opos: usize,
    ipos: usize,
    output: &mut [u8],
) -> eyre::Result<(u8, usize, usize)> {
    if ipos + 32 > BLOB_SIZE {
        bail!("Blob input too short");
    }
    if opos + 31 > output.len() {
        bail!("Output buffer too small");
    }

    // Two highest order bits of the first byte should always be 0
    if blob[ipos] & 0b1100_0000 != 0 {
        bail!("Invalid field element in blob");
    }

    // Copy bytes [ipos+1..ipos+32] into output[opos..opos+31]
    output[opos..opos + 31].copy_from_slice(&blob[ipos + 1..ipos + 32]);

    Ok((blob[ipos], opos + 32, ipos + 32))
}

/// Reassembles bytes from the encoded 6-bit chunks.
/// Updates the output position and returns the new position.
fn reassemble_bytes(
    mut opos: usize,
    encoded_byte: &[u8; 4],
    output: &mut [u8],
) -> eyre::Result<usize> {
    if opos < 96 {
        bail!("Output position too small for reassembly");
    }
    opos -= 1; // Account for the fact that we don't output a 128th byte

    let a = encoded_byte[0];
    let b = encoded_byte[1];
    let c = encoded_byte[2];
    let d = encoded_byte[3];

    // Reconstruct x, y, z from the encoded bytes
    let x = (a & 0b0011_1111) | ((b & 0b0011_0000) << 2);
    let y = (b & 0b0000_1111) | ((d & 0b0000_1111) << 4);
    let z = (c & 0b0011_1111) | ((d & 0b0011_0000) << 2);

    // Calculate positions
    let x_pos = opos - (32 * 3);
    let y_pos = opos - (32 * 2);
    let z_pos = opos - 32;

    // Ensure indices are valid
    if x_pos >= output.len() || y_pos >= output.len() || z_pos >= output.len() {
        bail!("Output position out of bounds during reassembly");
    }

    // Place the reassembled bytes into their appropriate output locations
    output[x_pos] = x;
    output[y_pos] = y;
    output[z_pos] = z;

    Ok(opos)
}
