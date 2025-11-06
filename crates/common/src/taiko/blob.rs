use alloy_consensus::BlobTransactionSidecar;
use alloy_eips::eip4844::env_settings::EnvKzgSettings;
use c_kzg::BYTES_PER_BLOB;

// panic if this fails
pub fn blobs_to_sidecar(blobs: Vec<c_kzg::Blob>) -> BlobTransactionSidecar {
    let kzg_settings = EnvKzgSettings::Default.get();

    let mut commitments = Vec::with_capacity(blobs.len());
    let mut proofs = Vec::with_capacity(blobs.len());

    for blob in blobs.iter() {
        let commitment = kzg_settings
            .blob_to_kzg_commitment(blob)
            .expect("failed to compute commitment")
            .to_bytes();
        let proof = kzg_settings
            .compute_blob_kzg_proof(blob, &commitment)
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
