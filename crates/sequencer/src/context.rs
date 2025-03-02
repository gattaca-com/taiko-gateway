use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use alloy_rpc_types::Header;
use pc_common::{
    sequencer::StateId,
    taiko::{AnchorParams, ParentParams},
    utils::verify_and_log_block,
};
use tracing::{debug, warn};

/// Sequencing state
pub struct SequencerContext {
    pub l1_safe_lag: u64,
    pub last_l1_receive: Instant,
    /// Current state
    pub state: SequencerState,
    /// Anchor data to use for next batches
    pub anchor: AnchorParams,
    /// L2 parent block info
    pub parent: ParentParams,
    /// Last confirmed L1 header, keep a buffer to account for L1 reorgs
    pub l1_headers: BTreeMap<u64, Header>,
    /// L2 blocks which have been posted on the L1
    pub to_verify: BTreeMap<u64, Header>,
    /// Last synced L2 block number
    pub l2_origin: Arc<AtomicU64>,
}

impl SequencerContext {
    pub fn new(l1_safe_lag: u64, l2_origin: Arc<AtomicU64>) -> Self {
        Self {
            l1_safe_lag,
            last_l1_receive: Instant::now(),
            state: SequencerState::default(),
            anchor: AnchorParams::default(),
            l1_headers: BTreeMap::new(),
            parent: Default::default(),
            to_verify: BTreeMap::new(),
            l2_origin,
        }
    }

    pub fn new_l1_block(&mut self, new_header: Header) {
        debug!(sync_time = ?self.last_l1_receive.elapsed(), number = new_header.number, hash = %new_header.hash, "new l1 block");

        self.l1_headers.retain(|n, _| *n >= new_header.number - self.l1_safe_lag);
        self.l1_headers.insert(new_header.number, new_header);

        self.last_l1_receive = Instant::now();
    }

    /// Insert a new preconfed L2 block, this could be sequenced by us or another gateway, we only
    /// verify our blocks
    pub fn new_preconf_l2_block(&mut self, new_header: &Header, ours: bool) {
        if new_header.number > self.parent.block_number {
            debug!(sequenced_locally = ours, number = new_header.number, hash = %new_header.hash, "new l2 preconf block");

            self.parent = ParentParams {
                timestamp: new_header.timestamp,
                gas_used: new_header.gas_used.try_into().unwrap(),
                block_number: new_header.number,
            };
        }

        if ours {
            self.to_verify.insert(new_header.number, new_header.clone());
        }
    }

    /// Process a new L2 block as confirmed by L1 batch transaction.
    /// This is always <= the preconf block (assuming remote RPC is well peered)
    pub fn new_origin_l2_block(&mut self, new_header: &Header) {
        debug!(number = new_header.number, hash = %new_header.hash, "new l2 origin block");

        if let Some(preconf_header) = self.to_verify.remove(&new_header.number) {
            verify_and_log_block(&preconf_header, new_header, true);
        } else if self
            .to_verify
            .first_key_value()
            .map(|(bn, _)| bn < &new_header.number)
            .unwrap_or(false)
        {
            // we missed some blocks, clear the map anyways to avoid keeping them here
            let n_before = self.to_verify.len();
            self.to_verify.retain(|bn, _| *bn > new_header.number);
            let n_after = self.to_verify.len();

            warn!(pruned = n_before - n_after, "missed some blocks, pruning verifications");
        }
    }

    pub fn safe_l1_header(&self) -> Option<&Header> {
        self.l1_headers.first_key_value().map(|(_, h)| h)
    }

    pub fn current_anchor_id(&self) -> u64 {
        self.anchor.block_id
    }

    pub fn l2_origin(&self) -> u64 {
        self.l2_origin.load(Ordering::Relaxed)
    }
}

#[derive(Debug, Clone, Default)]
pub enum SequencerState {
    /// Syncing L1/L2 blocks
    #[default]
    Sync,
    /// After simulating anchor tx, ready to sequence
    Anchor {
        /// Data used in anchor sim
        anchor_params: AnchorParams,
        /// Current block number (parent + 1)
        block_number: u64,
        /// Anchor state id
        state_id: StateId,
    },
    /// Sequencing user txs
    Sequence {
        /// Data used in anchor sim
        anchor_params: AnchorParams,
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
