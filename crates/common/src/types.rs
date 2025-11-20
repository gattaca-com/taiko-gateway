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
    UnderpricedGasFeeCap { sent: u128, queued: u128 },
    UnderpricedUnknown,
}

impl FailReason {
    pub fn try_extract(input: &str) -> Option<Self> {
        let lower = input.to_lowercase();

        let is_underpriced = lower.contains("replacement transaction underpriced");
        if !is_underpriced {
            return None;
        }

        if let Some(after) = lower.split("new tx gas tip cap ").nth(1) {
            let parts: Vec<&str> = after.split(" <").collect();
            if parts.len() != 2 {
                return None;
            }

            let sent = extract_first_u128(parts[0])?;
            let queued = extract_first_u128(parts[1])?;

            return Some(FailReason::UnderpricedTipCap { sent, queued });
        }

        if let Some(after) = lower.split("new tx blob gas fee cap ").nth(1) {
            let parts: Vec<&str> = after.split(" <").collect();
            if parts.len() != 2 {
                return None;
            }

            let sent = extract_first_u128(parts[0])?;
            let queued = extract_first_u128(parts[1])?;

            return Some(FailReason::UnderpricedBlobFeeCap { sent, queued });
        }

        if let Some(after) = lower.split("new tx gas fee cap ").nth(1) {
            let parts: Vec<&str> = after.split(" <").collect();
            if parts.len() != 2 {
                return None;
            }

            println!("Parts: {:?}", parts);

            let sent = extract_first_u128(parts[0])?;
            let queued = extract_first_u128(parts[1])?;

            return Some(FailReason::UnderpricedGasFeeCap { sent, queued });
        }

        if is_underpriced {
            return Some(FailReason::UnderpricedUnknown);
        }

        None
    }
}

fn extract_first_u128(s: &str) -> Option<u128> {
    let start = s.find(|c: char| c.is_ascii_digit())?;
    let remainder = &s[start..];
        let end = remainder
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(remainder.len());
    remainder[..end].parse::<u128>().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_first_u128() {
        assert_eq!(extract_first_u128("abc 12/2"), Some(12));
        assert_eq!(extract_first_u128("12"), Some(12));
        assert_eq!(extract_first_u128("<= 12"), Some(12));
        assert_eq!(extract_first_u128("12 3"), Some(12));
    }

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

    #[test]
    fn test_extract_fail_reason_blob_fee_cap_new() {
        let input = "server returned an error response: error code -32000: replacement transaction underpriced: new tx blob gas fee cap 1706792664 < 1321739451 queued + 100% replacement penalty";
        let result = FailReason::try_extract(input).unwrap();
        assert_eq!(FailReason::UnderpricedBlobFeeCap { sent: 1706792664, queued: 1321739451 }, result);
    }

    #[test]
    fn test_extract_fail_reason_blob_fee_cap_2() {
        let input = "server returned an error response: error code -32000: replacement transaction underpriced: new tx gas fee cap 2101629 < 1400047 queued + 100% replacement penalty";
        let result = FailReason::try_extract(input).unwrap();
        assert_eq!(FailReason::UnderpricedGasFeeCap { sent: 2101629, queued: 1400047 }, result);
    }

    #[test]
    fn test_extract_fail_reason_gas_fee_cap_equal() {
        let input = "server returned an error response: error code -32000: replacement transaction underpriced: new tx gas fee cap 1000000000 <= 1000000000 queued";
        let result = FailReason::try_extract(input).unwrap();
        assert_eq!(
            FailReason::UnderpricedGasFeeCap { sent: 1000000000, queued: 1000000000 },
            result
        );
    }
}
