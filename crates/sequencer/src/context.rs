use std::{collections::BTreeMap, time::Instant};

use alloy_rpc_types::Header;
use pc_common::{
    sequencer::StateId,
    taiko::{AnchorParams, ParentParams},
};
use tracing::debug;

/// Sequencing state
pub struct SequencerContext {
    pub l1_safe_lag: u64,
    pub l1_delayed: bool,
    pub last_l1_receive: Instant,
    /// Current state
    pub state: SequencerState,
    /// Anchor data to use for next batches
    pub anchor: AnchorParams,
    /// Either last preconfed or last from chain
    pub parent: ParentParams,
    /// Last confirmed L1 header, keep a buffer to account for L1 reorgs
    pub l1_headers: BTreeMap<u64, Header>,
}

impl SequencerContext {
    pub fn new(l1_safe_lag: u64) -> Self {
        Self {
            l1_safe_lag,
            l1_delayed: false,
            last_l1_receive: Instant::now(),
            state: SequencerState::default(),
            anchor: AnchorParams::default(),
            l1_headers: BTreeMap::new(),
            parent: Default::default(),
        }
    }

    pub fn new_l1_block(&mut self, new_header: Header) {
        debug!(sync_time = ?self.last_l1_receive.elapsed(), number = new_header.number, hash = %new_header.hash, "new l1 block");

        self.l1_headers.retain(|n, _| *n >= new_header.number - self.l1_safe_lag);
        self.l1_headers.insert(new_header.number, new_header);

        self.last_l1_receive = Instant::now();
    }

    // either preconf or not
    pub fn new_l2_block(&mut self, new_header: &Header) {
        debug!(number = new_header.number, hash = %new_header.hash, "new l2 block");

        if new_header.number > self.parent.block_number {
            // we should never receive a new block while we're sequencing
            assert!(matches!(self.state, SequencerState::Sync));

            self.parent = ParentParams {
                timestamp: new_header.timestamp,
                gas_used: new_header.gas_used.try_into().unwrap(),
                block_number: new_header.number,
            };
        }
    }

    pub fn safe_l1_header(&self) -> Option<&Header> {
        self.l1_headers.first_key_value().map(|(_, h)| h)
    }

    pub fn current_anchor_id(&self) -> u64 {
        self.anchor.block_id
    }
}

#[derive(Debug, Clone, Default)]
pub enum SequencerState {
    /// Syncing L1/L2 blocks
    #[default]
    Sync,
    /// After simulating anchor tx, ready to sequence
    Anchor {
        /// Anchor block id
        anchor_block_id: u64,
        /// Current block number (parent + 1)
        block_number: u64,
        /// Anchor state id
        state_id: StateId,
    },
    /// Sequencing user txs
    Sequence {
        /// Anchor block id
        anchor_block_id: u64,
        /// Current block number (parent + 1)
        block_number: u64,
        /// State to sequence txs on
        tip_state_id: StateId,

        /// Time when the first user tx was sequenced in the block
        start: Instant,
        /// Running total of gas used in the block
        running_gas_used: u128,
        /// number of user txs in the block
        num_txs: usize,
    },
}
