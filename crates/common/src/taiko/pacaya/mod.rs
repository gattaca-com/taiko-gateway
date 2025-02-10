mod anchor;
mod propose;

use alloy_sol_types::sol;
pub use anchor::*;
pub use propose::*;

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
