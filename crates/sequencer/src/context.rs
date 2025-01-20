use std::{
    collections::BTreeMap,
    time::{Duration, Instant},
};

use alloy_rpc_types::Header;
use pc_common::{sequencer::StateId, taiko::AnchorParams};

/// Sequencing state
pub struct SequencerContext {
    /// how long should l2 blocks last
    pub target_block_time: Duration,
    /// Env for block being built
    pub env: AnchorEnv,
    /// Data of anchor for this batch
    pub anchor: AnchorParams,
    /// Last confirmed L1 header, keep a buffer to account for L1 reorgs
    pub l1_headers: BTreeMap<u64, Header>,
    /// Either last preconfed or last from chain
    pub parent_l2_header: Header,
    // todo: add l2 block verification
}

/// Use as anchors blocks with this lag to make sure we dont use a reorged L1 block
pub const SAFE_L1_LAG: u64 = 4;

impl SequencerContext {
    pub fn new(target_block_time: Duration) -> Self {
        Self {
            target_block_time,
            env: AnchorEnv::default(),
            anchor: AnchorParams::default(),
            l1_headers: BTreeMap::new(),
            parent_l2_header: Default::default(),
        }
    }

    pub fn new_l1_block(&mut self, new_header: Header) {
        self.l1_headers.retain(|n, _| *n >= new_header.number - SAFE_L1_LAG);
        self.l1_headers.insert(new_header.number, new_header);
    }

    // either preconf or not
    pub fn new_l2_block(&mut self, new_header: Header) {
        assert!(new_header.number >= self.parent_l2_header.number);
        assert!(!self.is_building());

        self.parent_l2_header = new_header;
    }

    pub fn refresh_anchor(&mut self, new: AnchorParams) {
        self.anchor = new;
    }

    /// Add user transaction to the live block env
    pub fn sequence_tx(&mut self, new_state_id: StateId, gas_used: u128) {
        let anchor_env = &mut self.env;
        // advance local state
        anchor_env.tip_state_id = new_state_id;
        anchor_env.running_gas_used += gas_used;

        if anchor_env.num_txs == 0 {
            anchor_env.first_sequenced = Instant::now()
        }

        anchor_env.num_txs += 1;
    }

    pub fn is_building(&self) -> bool {
        self.env.num_txs > 0
    }

    pub fn should_seal(&self) -> bool {
        let seal_by_time = self.env.first_sequenced.elapsed() > self.target_block_time;
        let seal_by_gas = false;

        seal_by_time || seal_by_gas
    }

    pub fn current_anchor_id(&self) -> u64 {
        self.anchor.block_id
    }

    pub fn is_anchored(&self) -> bool {
        self.env.anchored
    }

    pub fn anchor_current_block(&mut self, block_number: u64, state_id: StateId) {
        self.env.anchored = true;
        self.env.block_number = block_number;
        self.env.tip_state_id = state_id;
    }
}

/// Block env with extra anchor data for the block being built, created only after a successful
/// anchor sim
#[derive(Debug, Clone)]
pub struct AnchorEnv {
    pub anchored: bool,
    /// Block number (parent + 1)
    pub block_number: u64,
    /// Time when the first user tx was sequenced in the block
    pub first_sequenced: Instant,
    /// Running total of gas used in the block
    pub running_gas_used: u128,
    /// State to sequence txs on
    pub tip_state_id: StateId,
    /// number of user txs in the block
    pub num_txs: usize,
}

impl Default for AnchorEnv {
    fn default() -> Self {
        Self {
            anchored: Default::default(),
            block_number: Default::default(),
            first_sequenced: Instant::now(),
            running_gas_used: Default::default(),
            tip_state_id: StateId::new(0),
            num_txs: Default::default(),
        }
    }
}
