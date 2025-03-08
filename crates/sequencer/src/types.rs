use std::{
    collections::HashMap,
    ops::{Deref, DerefMut},
    sync::Arc,
    time::Duration,
};

use alloy_primitives::Address;
use pc_common::{
    sequencer::{ExecutionResult, Order, StateId},
    taiko::AnchorParams,
};

use crate::sorting::SortData;

#[derive(Clone, Default)]
pub enum SequencerState {
    /// Syncing L1/L2 blocks
    #[default]
    Sync,
    /// After simulating anchor tx, ready to sequence
    Anchor { block_info: BlockInfo, state_id: StateId },
    /// Sequencing user txs
    Sorting(SortData),
}

impl std::fmt::Debug for SequencerState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SequencerState::Sync => write!(f, "Sync"),
            SequencerState::Anchor { block_info, state_id } => {
                write!(
                    f,
                    "Anchor: block_number: {:?}, state_id: {}",
                    block_info.block_number, state_id
                )
            }
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
    pub order: Arc<Order>,
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
    pub order: Arc<Order>,
}

/// A map from a sender address to a state nonce. Note that the nonce could be either the one from
/// the parent block or a greater one while sorting
#[derive(Debug, Clone, Default)]
pub struct StateNonces(pub HashMap<Address, u64>);

impl Deref for StateNonces {
    type Target = HashMap<Address, u64>;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl DerefMut for StateNonces {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}
