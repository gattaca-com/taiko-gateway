use std::sync::LazyLock;

use alloy_primitives::{address, b256, keccak256, Address, Bytes, B256, U256};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::{sol, SolValue};

mod anchor;
mod checks;
mod fixed_signer;
mod propose;

pub use anchor::*;
pub use checks::get_and_validate_config;
pub use propose::*;

// need to namespace to avoid clashing of some types with the same name
pub mod l1 {
    use alloy_sol_types::sol;
    sol!(
        #[derive(Debug, Eq, PartialEq)]
        #[sol(rpc)]
        #[allow(missing_docs)]
        TaikoL1,
        "../../abi/TaikoL1.json"
    );
}

pub mod l2 {
    use alloy_sol_types::sol;
    sol!(
        #[derive(Debug, Eq, PartialEq)]
        #[sol(rpc)]
        #[allow(missing_docs)]
        TaikoL2,
        "../../abi/TaikoL2.json"
    );
}

// From TaikoData.BlockParamsV2
sol! {
    #[derive(Debug, Default)]
    struct BlockParamsV2 {
        address proposer;
        address coinbase;
        bytes32 parentMetaHash;
        uint64 anchorBlockId; // NEW
        uint64 timestamp; // NEW
        uint32 blobTxListOffset; // NEW
        uint32 blobTxListLength; // NEW
        uint8 blobIndex; // NEW
    }
}

/// Golden touch is the key that needs to propose the anchor tx in every block
pub const GOLDEN_TOUCH_PRIVATE_KEY: B256 =
    b256!("92954368afd3caa1f3ce3ead0069c1af414054aefe1ef9aeacc1bf426222ce38");
pub const GOLDEN_TOUCH_ADDRESS: Address = address!("0000777735367b36bC9B61C50022d9D0700dB4Ec");

pub static GOLDEN_TOUCH_SIGNER: LazyLock<PrivateKeySigner> =
    LazyLock::new(|| PrivateKeySigner::from_bytes(&GOLDEN_TOUCH_PRIVATE_KEY).unwrap());

/// Gas limit of the anchor tx
// this is not really on-chain so we need to hardcode it for now
pub const ANCHOR_GAS_LIMIT: u64 = 250_000;

// https://github.com/taikoxyz/taiko-mono/blob/ontake_preconfs/packages/protocol/contracts/L1/libs/LibProposing.sol#L129
/// Block number where the anchor is included
pub fn get_difficulty(bn: u64) -> B256 {
    let bytes = ("TAIKO_DIFFICULTY", bn);
    keccak256(bytes.abi_encode_params())
}

pub fn get_extra_data(sharing_pct: u8) -> Bytes {
    let a: [u8; 32] = U256::from(sharing_pct).to_be_bytes();
    Bytes::from(a)
}

#[cfg(test)]
mod tests {

    use alloy_primitives::bytes;

    use super::*;

    #[test]
    fn test_golden_touch() {
        assert_eq!(GOLDEN_TOUCH_SIGNER.address(), GOLDEN_TOUCH_ADDRESS);
    }

    #[test]
    fn test_extra_data() {
        let extra_data = get_extra_data(75);
        assert_eq!(
            extra_data,
            bytes!("000000000000000000000000000000000000000000000000000000000000004b")
        );
    }
}
