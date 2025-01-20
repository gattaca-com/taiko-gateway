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
    }

    #[derive(Debug, Default)]
    struct BatchParams {
        /// The address of the proposer, which is set by the PreconfTaskManager if
        /// enabled; otherwise, it must be address(0).
        address proposer;
        /// The address that will receive the block rewards; defaults to the proposer's
        /// address if set to address(0).
        address coinbase;
        /// metahash of the previous BatchMetadata
        bytes32 parentMetaHash;
        uint64 anchorBlockId;
        bytes32 anchorInput;
        uint64 lastBlockTimestamp;
        uint32 txListOffset;
        uint32 txListSize;
        // The index of the first blob in this batch.
        uint8 firstBlobIndex;
        // The number of blobs in this batch. Blobs are initially concatenated and subsequently
        // decompressed via Zlib.
        uint8 numBlobs;
        bool revertIfNotFirstProposal;
        bytes32[] signalSlots;
        // Specifies the number of blocks to be generated from this batch.
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
