mod anchor;
mod forced;
mod propose;

use alloy_sol_types::sol;
pub use anchor::*;
use eyre::bail;
pub use forced::*;
use l1::TaikoL1::TaikoL1Errors;
use preconf::{PreconfRouter::PreconfRouterErrors, PreconfWhitelist::PreconfWhitelistErrors};
pub use propose::*;

use crate::utils::extract_revert_reason;

pub mod l1 {
    use alloy_sol_types::sol;
    sol!(
        #[derive(Debug, Eq, PartialEq)]
        #[sol(rpc)]
        #[allow(missing_docs)]
        TaikoL1,
        "../../abi/TaikoInbox.abi.json"
    );
}

pub mod preconf {
    use alloy_sol_types::sol;

    sol!(
        #[derive(Debug, Eq, PartialEq)]
        #[sol(rpc)]
        #[allow(missing_docs)]
        PreconfRouter,
        "../../abi/PreconfRouter.abi.json"
    );

    sol!(
        #[derive(Debug, Eq, PartialEq)]
        #[sol(rpc)]
        #[allow(missing_docs)]
        PreconfWhitelist,
        "../../abi/PreconfWhitelist.abi.json"
    );
}

pub mod wrapper {
    use alloy_sol_types::sol;

    sol!(
        #[derive(Debug, Eq, PartialEq)]
        #[sol(rpc)]
        #[allow(missing_docs)]
        TaikoWrapper,
        "../../abi/TaikoWrapper.abi.json"
    );
}

pub mod forced_inclusion {
    use alloy_sol_types::sol;
    sol!(
        #[derive(Debug, Eq, PartialEq)]
        #[sol(rpc)]
        #[allow(missing_docs)]
        ForcedInclusionStore,
        "../../abi/ForcedInclusionStore.abi.json"
    );
}

pub mod l2 {
    use alloy_sol_types::sol;
    sol!(
        #[derive(Debug, Eq, PartialEq)]
        #[sol(rpc)]
        #[allow(missing_docs)]
        TaikoL2,
        "../../abi/TaikoAnchor.abi.json"
    );
}

#[derive(Debug, Eq, PartialEq)]
pub enum RevertReason {
    TaikoL1(TaikoL1Errors),
    PreconfWhitelist(PreconfWhitelistErrors),
    PreconfRouter(PreconfRouterErrors),
}

impl TryFrom<&str> for RevertReason {
    type Error = eyre::Report;

    fn try_from(value: &str) -> Result<Self, Self::Error> {
        if let Some(err) = extract_revert_reason::<TaikoL1Errors>(value) {
            Ok(Self::TaikoL1(err))
        } else if let Some(err) = extract_revert_reason::<PreconfWhitelistErrors>(value) {
            Ok(Self::PreconfWhitelist(err))
        } else if let Some(err) = extract_revert_reason::<PreconfRouterErrors>(value) {
            Ok(Self::PreconfRouter(err))
        } else {
            bail!("unknown revert reason")
        }
    }
}

// From ITaikoInbox.sol
sol! {
    #[derive(Debug, Default)]
    struct BlockParams {
        // the max number of transactions in this block. Note that if there are not enough
        // transactions in calldata or blobs, the block will contains as many transactions as
        // possible.
        uint16 numTransactions;
        // For the first block in a batch,  the block timestamp is the batch params' `timestamp`
        // plus this time shift value;
        // For all other blocks in the same batch, the block timestamp is its parent block's
        // timestamp plus this time shift value.
        uint8 timeShift;
        // Signals sent on L1 and need to sync to this L2 block.
        bytes32[] signalSlots;
    }


    #[derive(Debug, Default)]
    struct BlobParams {
        // The hashes of the blob. Note that if this array is not empty.  `firstBlobIndex` and
        // `numBlobs` must be 0.
        bytes32[] blobHashes;
        // The index of the first blob in this batch.
        uint8 firstBlobIndex;
        // The number of blobs in this batch. Blobs are initially concatenated and subsequently
        // decompressed via Zlib.
        uint8 numBlobs;
        // The byte offset of the blob in the batch.
        uint32 byteOffset;
        // The byte size of the blob.
        uint32 byteSize;
        // The block number when the blob was created.
        uint64 createdIn;
    }

    #[derive(Debug, Default)]
    struct BatchParams {
        address proposer;
        address coinbase;
        bytes32 parentMetaHash;
        uint64 anchorBlockId;
        uint64 lastBlockTimestamp;
        bool revertIfNotFirstProposal;
        // Specifies the number of blocks to be generated from this batch.
        BlobParams blobParams;
        BlockParams[] blocks;
    }
}

impl From<super::BaseFeeConfig> for l2::LibSharedData::BaseFeeConfig {
    fn from(config: super::BaseFeeConfig) -> Self {
        Self {
            adjustmentQuotient: config.adjustment_quotient,
            sharingPctg: config.sharing_pctg,
            gasIssuancePerSecond: config.gas_issuance_per_second,
            minGasExcess: config.min_gas_excess,
            maxGasIssuancePerBlock: config.max_gas_issuance_per_block,
        }
    }
}

impl From<l1::LibSharedData::BaseFeeConfig> for super::BaseFeeConfig {
    fn from(config: l1::LibSharedData::BaseFeeConfig) -> Self {
        Self {
            adjustment_quotient: config.adjustmentQuotient,
            sharing_pctg: config.sharingPctg,
            gas_issuance_per_second: config.gasIssuancePerSecond,
            min_gas_excess: config.minGasExcess,
            max_gas_issuance_per_block: config.maxGasIssuancePerBlock,
        }
    }
}
