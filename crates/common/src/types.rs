use alloy_primitives::{Address, B256, U256};
use serde::{Deserialize, Serialize};

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AnchorData {
    /// Anchor block id, a recent L1 block number
    pub block_id: u64,
    /// Timestamp of the L2 block, need not be = timestamp of anchor block id
    pub timestamp: u64,
    /// State root of the block id
    pub state_root: B256,
    /// Base fee
    pub base_fee: u128,
}

// Execution Block env from revm
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockEnv {
    /// The number of ancestor blocks of this block (block height).
    pub number: U256,
    /// Coinbase or miner or address that created and signed the block.
    ///
    /// This is the receiver address of all the gas spent in the block.
    pub coinbase: Address,

    /// The timestamp of the block in seconds since the UNIX epoch.
    pub timestamp: U256,
    /// The gas limit of the block.
    pub gas_limit: U256,
    /// The base fee per gas, added in the London upgrade with [EIP-1559].
    ///
    /// [EIP-1559]: https://eips.ethereum.org/EIPS/eip-1559
    pub basefee: U256,
    /// The difficulty of the block.
    ///
    /// Unused after the Paris (AKA the merge) upgrade, and replaced by `prevrandao`.
    pub difficulty: U256,
    /// The output of the randomness beacon provided by the beacon chain.
    ///
    /// Replaces `difficulty` after the Paris (AKA the merge) upgrade with [EIP-4399].
    ///
    /// NOTE: `prevrandao` can be found in a block in place of `mix_hash`.
    ///
    /// [EIP-4399]: https://eips.ethereum.org/EIPS/eip-4399
    pub prevrandao: Option<B256>,
    /// Excess blob gas and blob gasprice.
    /// See also [`crate::calc_excess_blob_gas`]
    /// and [`calc_blob_gasprice`].
    ///
    /// Incorporated as part of the Cancun upgrade via [EIP-4844].
    ///
    /// [EIP-4844]: https://eips.ethereum.org/EIPS/eip-4844
    pub blob_excess_gas_and_price: Option<()>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum FailReason {
    UnderpricedTipCap { sent: u128, queued: u128 },
    UnderpricedBlobFeeCap { sent: u128, queued: u128 },
}

impl FailReason {
    pub fn try_extract(input: &str) -> Option<Self> {
        let lower = input.to_lowercase();

        if !lower.contains("replacement transaction underpriced") {
            return None;
        }

        if let Some(after) = lower.split("new tx gas tip cap ").nth(1) {
            let parts: Vec<&str> = after.split(" <= ").collect();
            if parts.len() != 2 {
                return None;
            }

            let sent = parts[0].trim().parse::<u128>().ok()?;

            let queued_str = parts[1].split_whitespace().next()?;
            let queued = queued_str.parse::<u128>().ok()?;

            return Some(FailReason::UnderpricedTipCap { sent, queued });
        }

        if let Some(after) = lower.split("new tx blob gas fee cap ").nth(1) {
            let parts: Vec<&str> = after.split(" <= ").collect();
            if parts.len() != 2 {
                return None;
            }

            let sent = parts[0].trim().parse::<u128>().ok()?;

            let queued_str = parts[1].split_whitespace().next()?;
            let queued = queued_str.parse::<u128>().ok()?;

            return Some(FailReason::UnderpricedBlobFeeCap { sent, queued });
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_fail_reason() {
        let input = "server returned an error response: error code -32000: replacement transaction underpriced: new tx gas tip cap 120000000 <= 530716904 queued";
        let result = FailReason::try_extract(input).unwrap();
        assert_eq!(FailReason::UnderpricedTipCap { sent: 120000000, queued: 530716904 }, result);
    }

    #[test]
    fn test_extract_fail_reason_blob_fee_cap() {
        let input = "server returned an error response: error code -32000: replacement transaction underpriced: new tx blob gas fee cap 1 <= 2 queued";
        let result = FailReason::try_extract(input).unwrap();
        assert_eq!(FailReason::UnderpricedBlobFeeCap { sent: 1, queued: 2 }, result);
    }
}
