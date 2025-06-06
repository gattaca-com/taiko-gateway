use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    time::Duration,
};

use alloy_primitives::{Address, B256};
use alloy_rpc_types::{Block, Header};
use crossbeam_channel::Receiver;
use pc_common::{
    metrics::SequencerMetrics,
    proposer::ProposalRequest,
    sequencer::{ExecutionResult, Order, StateId},
    taiko::AnchorParams,
};
use tokio::sync::mpsc::UnboundedSender;

use crate::sorting::SortData;

pub struct SequencerSpine {
    /// Receive txs and bundles from RPC
    pub rpc_rx: Receiver<Order>,
    /// Receive txs from mempool
    pub mempool_rx: Receiver<Order>,
    /// Send blocks to proposer for inclusion
    pub proposer_tx: UnboundedSender<ProposalRequest>,
    // Receiver of L1 blocks
    pub l1_blocks_rx: Receiver<Header>,
    // Receiver of L2 preconf blocks
    pub l2_blocks_rx: Receiver<Block>,
    // Receiver of L2 non preconf blocks
    pub origin_blocks_rx: Receiver<Header>,
    /// Receive sim results
    pub sim_rx: Receiver<eyre::Result<SimulatedOrder>>,
}

#[derive(Clone)]
pub enum SequencerState {
    /// Syncing L1/L2 blocks
    Sync { last_l1: u64 },
    /// Simualted anchor and sequencing user txs
    Sorting(SortData),
}

impl Default for SequencerState {
    fn default() -> Self {
        Self::Sync { last_l1: 0 }
    }
}

impl SequencerState {
    pub fn record_metrics(&self) {
        let state_id = match self {
            SequencerState::Sync { .. } => 0,
            SequencerState::Sorting(..) => 2,
        };

        SequencerMetrics::set_state(state_id);
    }
}

impl std::fmt::Debug for SequencerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SequencerState::Sync { .. } => write!(f, "Sync"),
            SequencerState::Sorting(sort_data) => {
                write!(
                    f,
                    "Sorting: block_number {:?}, state_id: {}",
                    sort_data.block_info.block_number, sort_data.state_id
                )
            }
        }
    }
}

#[derive(Debug, Copy, Clone)]
pub struct BlockInfo {
    /// Data used in anchor sim
    pub anchor_params: AnchorParams,
    /// Current block number (parent + 1)
    pub block_number: u64,
    /// Base fee
    pub base_fee: u128,
    /// Parent hash
    pub parent_hash: B256,
}

#[derive(Debug, Clone)]
pub struct SimulatedOrder {
    /// What state id this was simulated on
    pub origin_state_id: StateId,
    /// Result of the simulation
    pub execution_result: ExecutionResult,
    /// Simulation time
    pub sim_time: Duration,
    /// Original order
    pub order: Order,
}

/// Order that is valid and can be applied to the block
#[derive(Debug, Clone)]
pub struct ValidOrder {
    /// What state id this was simulated on
    pub origin_state_id: StateId,
    /// State id after applying the order
    pub new_state_id: StateId,
    /// Gas used by the order
    pub gas_used: u128,
    /// Builder payment for the order
    pub builder_payment: u128,
    /// Original order
    pub order: Order,
}

/// A map from a sender address to a state nonce
#[derive(Debug, Default, Clone)]
pub struct StateNonces {
    pub nonces: HashMap<Address, u64>,
    /// The nonces are valid to be used for this block number
    pub valid_block: u64,
}

impl StateNonces {
    pub fn new(valid_block: u64) -> Self {
        Self { nonces: HashMap::new(), valid_block }
    }

    pub fn is_valid_parent(&self, parent_block: u64) -> bool {
        self.valid_block == parent_block + 1
    }

    pub fn is_valid_block(&self, block_number: u64) -> bool {
        self.valid_block == block_number
    }
}

impl Deref for StateNonces {
    type Target = HashMap<Address, u64>;

    fn deref(&self) -> &Self::Target {
        &self.nonces
    }
}

impl DerefMut for StateNonces {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.nonces
    }
}

#[derive(Debug, Clone)]
pub struct SortingNonces {
    pub state: StateNonces,
    pub sorting: StateNonces,
}

impl SortingNonces {
    pub fn new(valid_block: u64) -> Self {
        Self { state: StateNonces::new(valid_block), sorting: StateNonces::new(valid_block) }
    }

    pub fn get_nonce(&self, sender: &Address) -> Option<&u64> {
        self.sorting.get(sender).or_else(|| self.state.get(sender))
    }

    pub fn insert_state(&mut self, sender: Address, nonce: u64) {
        self.state.insert(sender, nonce);
    }

    pub fn insert_sorting(&mut self, sender: Address, nonce: u64) {
        self.sorting.insert(sender, nonce);
    }

    pub fn valid_block(&self) -> u64 {
        self.state.valid_block
    }

    pub fn all_iter(self) -> impl Iterator<Item = (Address, u64)> {
        self.state.nonces.into_iter().chain(self.sorting.nonces)
    }

    pub fn all_merged(mut self) -> StateNonces {
        self.state.nonces.extend(self.sorting.nonces);
        self.state
    }
}
