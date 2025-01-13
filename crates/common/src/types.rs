use alloy_primitives::{Address, B256, U256};
use crossbeam_channel::{Receiver, Sender};
use serde::{Deserialize, Serialize};

pub struct DuplexChannel<T, V> {
    pub tx: Sender<T>,
    pub rx: Receiver<V>,
}

impl<Req, Res> DuplexChannel<Req, Res> {
    pub fn new() -> (Self, DuplexChannel<Res, Req>) {
        let (req_tx, req_rx) = crossbeam_channel::unbounded();
        let (res_tx, res_rx) = crossbeam_channel::unbounded();

        (Self { tx: req_tx, rx: res_rx }, DuplexChannel { tx: res_tx, rx: req_rx })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
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
