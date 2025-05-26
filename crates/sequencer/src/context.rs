use std::{
    collections::{BTreeMap, VecDeque},
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Instant,
};

use alloy_consensus::Transaction;
use alloy_primitives::Address;
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
    pub coinbase: Address,
    pub l1_safe_lag: u64,
    pub last_l1_receive: Instant,
    pub last_l2_receive: Instant,
    /// Current state
    pub state: SequencerState,
    /// Anchor data to use for next batches
    pub anchor: Option<AnchorParams>,
    /// L2 blocks sorted by block number, we keep all the unproposed blocks >= L2 origin
    /// this should never be empty after startup
    pub l2_headers: VecDeque<ParentParams>,
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
    pub fn new(
        l1_safe_lag: u64,
        l2_origin: Arc<AtomicU64>,
        l1_number: Arc<AtomicU64>,
        coinbase: Address,
    ) -> Self {
        Self {
            l1_safe_lag,
            last_l1_receive: Instant::now(),
            last_l2_receive: Instant::now(),
            state: SequencerState::default(),
            anchor: None,
            l1_headers: BTreeMap::new(),
            l2_headers: VecDeque::with_capacity(400),
            to_verify: BTreeMap::new(),
            l2_origin,
            l1_number,
            parent_anchor_block_id: 0,
            coinbase,
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
    /// returns whether a reorg happened
    pub fn new_preconf_l2_block(&mut self, new_block: &Block) -> bool {
        let header = &new_block.header;

        if header.beneficiary == self.coinbase {
            self.to_verify.insert(header.number, header.clone());
        }

        let Some(last_seen) = self.l2_headers.back() else {
            // first block
            self.update_parent_block_id(new_block);
            return false;
        };

        if last_seen.block_number + 1 == header.number && last_seen.hash == header.parent_hash {
            // happy case, we have the previous block
            debug!(
                number = header.number,
                n_txs = new_block.transactions.len(),
                sync_time = ?self.last_l2_receive.elapsed(),
                hash = %header.hash,
                coinbase = %header.beneficiary,
                parent_hash = %last_seen.hash,
                "new l2 preconf block"
            );

            self.update_parent_block_id(new_block);
            return false;
        }

        // from here: either we have already this block, we missed some blocks, or we have a reorg
        if let Some(local) = self.l2_headers.iter().position(|p| p.block_number == header.number) {
            if self.l2_headers[local].hash != header.hash {
                // reorg, reset all following blocks

                debug!(
                    n_txs = new_block.transactions.len(),
                    sync_time = ?self.last_l2_receive.elapsed(),
                    new_hash = %header.hash,
                    reorged_hash = %self.l2_headers[local].hash,
                    coinbase = %header.beneficiary,
                    "l2 reorg: {} -> {}", header.number, last_seen.block_number
                );
                self.l2_headers.truncate(local); // remove local too

                // future blocks will anyways fail verification
                let to_verify_len = self.to_verify.len();
                self.to_verify.retain(|bn, _| *bn < header.number);
                let to_verify_len_after = self.to_verify.len();
                if to_verify_len - to_verify_len_after > 0 {
                    warn!(
                        pruned = to_verify_len - to_verify_len_after,
                        "pruned verifications due to reorg"
                    );
                }
                self.update_parent_block_id(new_block);
                return true;
            } else {
                // saw this block before
            }
        } else {
            // missed some blocks
            if last_seen.block_number < header.number {
                warn!(
                    number = header.number,
                    last_seen = last_seen.block_number,
                    n_txs = new_block.transactions.len(),
                    sync_time = ?self.last_l2_receive.elapsed(),
                    hash = %header.hash,
                    parent_hash = %last_seen.hash,
                    coinbase = %header.beneficiary,
                    "new l2 preconf block (missed blocks)"
                );

                self.update_parent_block_id(new_block);
            } else {
                // missed some previous blocks, the parent is now potentially on
                // the wrong fork
                warn!(
                    number = header.number,
                    last_seen = last_seen.block_number,
                    n_txs = new_block.transactions.len(),
                    sync_time = ?self.last_l2_receive.elapsed(),
                    hash = %header.hash,
                    parent_hash = %last_seen.hash,
                    coinbase = %header.beneficiary,
                    "new l2 preconf block may cause an unhandled reorg"
                );
            }
        }

        false
    }

    fn update_parent_block_id(&mut self, block: &Block) {
        if let Err(err) = (|| -> eyre::Result<()> {
            BlocksMetrics::l2_block(&block.header);
            self.l2_headers.push_back((&block.header).into());
            let block_transactions =
                block.transactions.as_transactions().context("block has txs")?;
            let block_anchor = block_transactions.first().context("block has anchor tx")?;
            assert_eq!(block_anchor.from, GOLDEN_TOUCH_ADDRESS);

            let anchor_call = anchorV3Call::abi_decode(block_anchor.input(), true)?;
            self.parent_anchor_block_id = anchor_call._anchorBlockId;

            Ok(())
        })() {
            error!(%err, "failed to update parent block id");
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

        // prune l2 blocks
        while let Some(l2_block) = self.l2_headers.front() {
            // keep last origin block
            if l2_block.block_number < new_header.number {
                self.l2_headers.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn safe_l1_header(&self) -> Option<&Header> {
        self.l1_headers.first_key_value().map(|(_, h)| h)
    }

    pub fn l2_origin(&self) -> u64 {
        self.l2_origin.load(Ordering::Relaxed)
    }

    pub fn l2_parent(&self) -> &ParentParams {
        self.l2_headers.back().expect("l2 headers should never be empty")
    }
}
