use std::{
    collections::BTreeMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use alloy_consensus::Transaction;
use alloy_rpc_types::{Block, Header};
use alloy_sol_types::SolCall;
use eyre::ContextCompat;
use pc_common::{
    metrics::{BlocksMetrics, SequencerMetrics},
    taiko::{pacaya::l2::TaikoL2::anchorV3Call, AnchorParams, ParentParams, GOLDEN_TOUCH_ADDRESS},
    utils::verify_and_log_block,
};
use tracing::{debug, error, warn};

use crate::types::SequencerState;

/// Sequencing state
pub struct SequencerContext {
    pub l1_safe_lag: u64,
    pub last_l1_receive: Instant,
    /// Current state
    pub state: SequencerState,
    /// Anchor data to use for next batches
    pub anchor: Option<AnchorParams>,
    /// L2 parent block info
    pub parent: ParentParams,
    /// Last confirmed L1 header, keep a buffer to account for L1 reorgs
    pub l1_headers: BTreeMap<u64, Header>,
    /// L2 blocks which have been posted on the L1
    pub to_verify: BTreeMap<u64, Header>,
    /// Last synced L2 block number
    pub l2_origin: Arc<AtomicU64>,
    /// Last synced L1 block number
    pub l1_number: Arc<AtomicU64>,
    /// Anchor block id of the parent block
    pub parent_anchor_block_id: u64,
}

impl SequencerContext {
    pub fn new(l1_safe_lag: u64, l2_origin: Arc<AtomicU64>, l1_number: Arc<AtomicU64>) -> Self {
        Self {
            l1_safe_lag,
            last_l1_receive: Instant::now(),
            state: SequencerState::default(),
            anchor: None,
            l1_headers: BTreeMap::new(),
            parent: Default::default(),
            to_verify: BTreeMap::new(),
            l2_origin,
            l1_number,
            parent_anchor_block_id: 0,
        }
    }

    pub fn new_l1_block(&mut self, new_header: Header) {
        BlocksMetrics::l1_block_number(new_header.number);
        SequencerMetrics::set_l1_sync_time(self.last_l1_receive.elapsed());

        debug!(sync_time = ?self.last_l1_receive.elapsed(), number = new_header.number, hash = %new_header.hash, "new l1 block");

        self.l1_number.store(new_header.number, Ordering::Relaxed);
        self.l1_headers.retain(|n, _| *n >= new_header.number - self.l1_safe_lag);
        self.l1_headers.insert(new_header.number, new_header);

        self.last_l1_receive = Instant::now();
    }

    /// Insert a new preconfed L2 block, this could be sequenced by us or another gateway
    /// we only verify our blocks
    pub fn new_preconf_l2_block(&mut self, new_block: &Block, ours: bool) {
        let header = &new_block.header;

        if header.number > self.parent.block_number {
            BlocksMetrics::l2_block(header);

            debug!(sequenced_locally = ours, number = header.number, hash = %header.hash, "new l2 preconf block");

            self.parent = ParentParams {
                timestamp: header.timestamp,
                gas_used: header.gas_used.try_into().unwrap(),
                block_number: header.number,
                hash: header.hash,
            };
            if let Err(err) = self.update_parent_block_id(new_block) {
                error!(%err, "failed to update parent block id");
            }
        } else if header.number == self.parent.block_number && header.hash != self.parent.hash {
            warn!(sequenced_locally = ours, number = header.number, new_hash = %header.hash, old_hash = %self.parent.hash, "l2 reorg");

            self.parent = ParentParams {
                timestamp: header.timestamp,
                gas_used: header.gas_used.try_into().unwrap(),
                block_number: header.number,
                hash: header.hash,
            };
            if let Err(err) = self.update_parent_block_id(new_block) {
                error!(%err, "failed to update parent block id");
            }
        }

        if ours {
            self.to_verify.insert(header.number, header.clone());
        }
    }

    fn update_parent_block_id(&mut self, block: &Block) -> eyre::Result<()> {
        let block_transactions = block.transactions.as_transactions().context("block has txs")?;
        let block_anchor = block_transactions.first().context("block has anchor tx")?;
        assert_eq!(block_anchor.from, GOLDEN_TOUCH_ADDRESS);

        let anchor_call = anchorV3Call::abi_decode(&block_anchor.input(), true)?;

        let anchor_block_id = anchor_call._anchorBlockId;
        self.parent_anchor_block_id = anchor_block_id;

        Ok(())
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
        let (_, safe_header) = self.l1_headers.first_key_value()?;
        (safe_header.number >= self.parent_anchor_block_id).then_some(safe_header)
    }

    pub fn l2_origin(&self) -> u64 {
        self.l2_origin.load(Ordering::Relaxed)
    }
}
