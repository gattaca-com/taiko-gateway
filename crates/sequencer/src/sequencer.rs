//! Manages communication with simulator and sequence incoming transactions

use std::{
    sync::Arc,
    thread::sleep,
    time::{Duration, Instant},
};

use alloy_consensus::Transaction;
use alloy_primitives::{utils::format_ether, Address, U256};
use alloy_rpc_types::{Block, Header};
use crossbeam_channel::Receiver;
use eyre::bail;
use pc_common::{
    config::{L2ChainConfig, SequencerConfig, TaikoChainParams, TaikoConfig}, fetcher::fetch_latest_n_blocks_sync, proposer::{is_propose_delayed, is_resyncing, set_resyncing, ProposerEvent}, runtime::spawn, sequencer::{ExecutionResult, Order, StateId}, taiko::{get_difficulty, get_extra_data, AnchorParams, ANCHOR_GAS_LIMIT}, types::BlockEnv
};
use reqwest::Client;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, warn, Instrument};

use crate::{
    context::{SequencerContext, SequencerState},
    simulator::SimulatorClient,
    soft_block::BuildPreconfBlockRequestBody,
    txpool::TxPool,
};

pub struct Sequencer {
    config: SequencerConfig,
    chain_config: TaikoChainParams,
    l2_config: L2ChainConfig,
    ctx: SequencerContext,
    simulator: SimulatorClient,
    /// Receive txs and bundles from RPC
    rpc_rx: Receiver<Order>,
    /// Mempool transactions
    txpool: TxPool,
    /// Send blocks to proposer for inclusion
    proposer_events_tx: UnboundedSender<ProposerEvent>,
    // Receiver of L1 blocks
    l1_blocks_rx: Receiver<Header>,
    // Receiver of L2 non preconf blocks
    l2_blocks_rx: Receiver<Header>,
}

impl Sequencer {
    pub fn new(
        config: SequencerConfig,
        taiko_config: TaikoConfig,
        l2_config: L2ChainConfig,
        rpc_rx: Receiver<Order>,
        mempool_rx: Receiver<Order>,
        new_blocks_tx: UnboundedSender<ProposerEvent>,
        l1_blocks_rx: Receiver<Header>,
        l2_blocks_rx: Receiver<Header>,
    ) -> Self {
        let chain_config = taiko_config.params;
        let simulator = SimulatorClient::new(config.simulator_url.clone(), taiko_config);

        // this doesn't handle well the restarts if we had pending orders in the rpc
        let ctx = SequencerContext::new(config.l1_safe_lag);
        let txpool = TxPool::new(mempool_rx);

        Self {
            config,
            chain_config,
            l2_config,
            ctx,
            simulator,
            rpc_rx,
            txpool,
            proposer_events_tx: new_blocks_tx,
            l1_blocks_rx,
            l2_blocks_rx,
        }
    }

    #[tracing::instrument(skip_all, name = "sequencer")]
    pub fn run(mut self) {
        info!("waiting for block fetch");

        while !self.is_ready() {
            self.recv_blocks();
            sleep(Duration::from_millis(250));
        }

        info!("starting loop");

        loop {
            // fetch new data
            self.recv_blocks();
            self.txpool.fetch();
            self.maybe_refresh_anchor();

            // state transition
            match self.ctx.state {
                SequencerState::Sync => {
                    if !self.can_sequence() {
                        continue;
                    }

                    match self.anchor_block() {
                        Ok(state_id) => {
                            let block_number = self.ctx.parent.block_number + 1;
                            let anchor_params = self.ctx.anchor;
                            debug!(block_number, %state_id, anchor = ?anchor_params, "anchored");

                            self.ctx.state =
                                SequencerState::Anchor { anchor_params, block_number, state_id };
                        }

                        Err(err) => error!(%err, "failed anchoring"),
                    };
                }

                SequencerState::Anchor { anchor_params, block_number, state_id } => {
                    if !self.can_sequence() {
                        self.ctx.state = SequencerState::Sync;
                        continue;
                    }

                    if anchor_params.block_id != self.ctx.anchor.block_id {
                        // refresh with new anchor
                        match self.anchor_block() {
                            Ok(state_id) => {
                                let block_number = self.ctx.parent.block_number + 1;
                                let anchor_params = self.ctx.anchor;
                                debug!(block_number, %state_id, anchor = ?anchor_params, "re-anchored");

                                self.ctx.state = SequencerState::Anchor {
                                    anchor_params,
                                    block_number,
                                    state_id,
                                };
                            }

                            Err(err) => {
                                error!(%err, "failed re-anchoring");
                                self.ctx.state = SequencerState::Sync;
                            }
                        };
                    } else if let Some(order) = self.next_order() {
                        if let Some((new_state_id, gas_used)) = self.simulate_tx(state_id, order) {
                            self.ctx.state = SequencerState::Sequence {
                                anchor_params,
                                block_number,
                                tip_state_id: new_state_id,
                                start: Instant::now(),
                                running_gas_used: gas_used,
                                num_txs: 1,
                            };
                        }
                    }
                }

                SequencerState::Sequence {
                    anchor_params,
                    block_number,
                    start,
                    running_gas_used,
                    tip_state_id,
                    num_txs,
                } => {
                    let seal_by_time = start.elapsed() > self.config.target_block_time;
                    let seal_by_gas = false;

                    if seal_by_time || seal_by_gas || !self.can_sequence() {
                        if let Err(err) = self.commit_seal(tip_state_id, start, anchor_params) {
                            error!(%err, "failed commit seal");
                            self.initiate_resync();
                        } else {
                            // reset state for next block
                            self.ctx.state = SequencerState::Sync;
                        }
                    } else if let Some(order) = self.next_order() {
                        if let Some((new_state_id, gas_used)) =
                            self.simulate_tx(tip_state_id, order)
                        {
                            self.ctx.state = SequencerState::Sequence {
                                anchor_params,
                                block_number,
                                tip_state_id: new_state_id,
                                start,
                                running_gas_used: running_gas_used + gas_used,
                                num_txs: num_txs + 1,
                            };
                        }
                    }
                }
            }
        }
    }

    // TODO: this need to be aware of the preconf schedule
    fn can_sequence(&mut self) -> bool {
        if is_propose_delayed() || is_resyncing() {
            return false;
        }

        if self.ctx.last_l1_receive.elapsed() > self.config.l1_delay {
            if !self.ctx.l1_delayed {
                warn!("l1 block fetch is delayed, stopping sequencing");
                self.ctx.l1_delayed = true;
            }
        } else if self.ctx.l1_delayed {
            warn!("l1 block fetch has resumed");
            self.ctx.l1_delayed = false;
        }

        !self.ctx.l1_delayed
    }

    // TODO: if sim fails, rpc orders are lost
    fn next_order(&mut self) -> Option<Order> {
        self.rpc_rx.try_recv().ok().or(self.txpool.next_sequence())
    }

    fn recv_blocks(&mut self) {
        if let Ok(new_header) = self.l1_blocks_rx.try_recv() {
            self.ctx.new_l1_block(new_header);
        }

        if let Ok(new_header) = self.l2_blocks_rx.try_recv() {
            if !self.ctx.new_l2_block(&new_header) {
                // Verification failed, trigger resync
                warn!("block verification failed, stopping sequencing");
                self.initiate_resync();
            }
        }
    }

    fn is_ready(&self) -> bool {
        self.ctx.l1_headers.len() as u64 >= self.config.l1_safe_lag &&
            self.ctx.parent.block_number > 0
    }

    fn maybe_refresh_anchor(&mut self) {
        let Some(safe_l1_header) = self.ctx.safe_l1_header() else {
            error!("missing l1 headers");
            return;
        };

        // if current anchor has been used for enough blocks, refresh it
        let should_refresh_anchor =
            self.ctx.current_anchor_id() + self.config.anchor_batch_lag < safe_l1_header.number;

        if should_refresh_anchor {
            let new = AnchorParams {
                block_id: safe_l1_header.number,
                state_root: safe_l1_header.state_root,
                timestamp: self.ctx.parent.timestamp.max(safe_l1_header.timestamp),
            };

            self.ctx.anchor = new;

            debug!(anchor =? self.ctx.anchor, "refreshed anchor params");
        }
    }

    /// Anchors the current block
    fn anchor_block(&self) -> eyre::Result<StateId> {
        debug!(anchor =? self.ctx.anchor, parent =? self.ctx.parent, "assembling anchor");

        let (tx, l2_base_fee) = self.simulator.assemble_anchor(self.ctx.parent, self.ctx.anchor)?;

        let block_number = self.ctx.parent.block_number + 1;
        let block_env = get_block_env_from_anchor(
            block_number,
            self.chain_config.block_max_gas_limit,
            self.config.coinbase_address,
            self.ctx.anchor.timestamp,
            l2_base_fee,
        );

        let extra_data = get_extra_data(self.chain_config.base_fee_config.sharing_pctg);

        match self.simulator.simulate_anchor(tx, block_env, extra_data) {
            Ok(res) => {
                let ExecutionResult::Success { state_id, gas_used, builder_payment } = res else {
                    bail!("failed simulate anchor, res={res:?}");
                };

                debug!(anchor = ?self.ctx.anchor, parent = ?self.ctx.parent, gas_used, builder_payment, "simulated anchor");
                Ok(state_id)
            }
            Err(err) => {
                bail!("failed simulate anchor, err={err}")
            }
        }
    }

    fn simulate_tx(&self, origin_state_id: StateId, order: Order) -> Option<(StateId, u128)> {
        let hash = *order.tx_hash();

        match self.simulator.simulate_tx(order, origin_state_id) {
            Ok(res) => match res {
                ExecutionResult::Success { state_id: new_state_id, gas_used, builder_payment } => {
                    debug!(
                        %hash,
                        %origin_state_id,
                        %new_state_id,
                        gas_used,
                        payment = format_ether(builder_payment),
                        "sim successful"
                    );

                    return Some((new_state_id, gas_used));
                }
                ExecutionResult::Revert { .. } => {
                    debug!(%hash, %origin_state_id, "reverted user tx");
                }
                ExecutionResult::Invalid { reason } => {
                    // TODO: dedup txpool nonces here
                    debug!(%hash, %origin_state_id, reason, "invalid user tx");
                }
            },

            Err(err) => {
                error!(%err, "failed simulate tx")
            }
        }

        None
    }

    fn commit_seal(
        &mut self,
        origin_state_id: StateId,
        start_block: Instant,
        anchor_params: AnchorParams,
    ) -> eyre::Result<()> {
        // seal
        let res = self.simulator.seal_block(origin_state_id)?;

        let block = res.built_block;
        let block_number = block.header.number;

        info!(
            bn = block_number,
            block_hash = %block.header.hash,
            block_time = ?start_block.elapsed(),
            payment = format_ether(res.cumulative_builder_payment),
            gas_used = res.cumulative_gas_used,
            gas_limit = block.header.gas_limit,
            "sealed block"
        );

        let txs = block.transactions.txns().map(|tx| (tx.from, tx.nonce()));
        self.txpool.clear_mined(txs);

        self.ctx.new_preconf_l2_block(&block.header);
        
        info!("skipping gossiping soft block");
        //self.gossip_soft_block(block.clone(), anchor_params);

        self.send_block_to_proposer(block, anchor_params.block_id);

        Ok(())
    }

    fn send_block_to_proposer(&self, block: Arc<Block>, anchor_block_id: u64) {
        if let Err(err) = self.proposer_events_tx.send(ProposerEvent::SealedBlock { block, anchor_block_id }) {
            error!(%err, "failed sequencer->proposer");
        };
    }

    #[tracing::instrument(skip_all, name = "soft_blocks", fields(block = block.header.number))]
    fn gossip_soft_block(&self, block: Arc<Block>, anchor_params: AnchorParams) {
        let request = BuildPreconfBlockRequestBody::new(
            block,
            anchor_params,
            self.chain_config.base_fee_config,
        );

        let url = self.config.soft_block_url.clone();
        spawn(
            async move {
                let raw = serde_json::to_string(&request).unwrap();
                match Client::new().post(url).json(&request).send().await {
                    Ok(res) => {
                        let status = res.status();
                        let body = res.text().await.unwrap();

                        if status.is_success() {
                            debug!("soft block posted: response {}, raw {}", body, raw);
                        } else {
                            error!(code = status.as_u16(), err = body, %raw, "soft block failed");
                        }
                    }
                    Err(err) => {
                        error!(%err, %raw, "failed to post soft block")
                    }
                }
            }
            .in_current_span(),
        );
    }

    fn initiate_resync(&mut self) {
        info!("initiating resync");
        set_resyncing();
        
        // Reset context to initial state
        info!(
            old_state = ?self.ctx,
            "resetting context to initial state"
        );
        self.ctx = SequencerContext::new(self.config.l1_safe_lag);
        self.ctx.state = SequencerState::Sync;

        // Signal to proposer that resync is needed
        if let Err(err) = self.proposer_events_tx.send(ProposerEvent::NeedsResync) {
            error!(%err, "failed to signal resync needed - channel may be disconnected");
        }

        info!(
            l1_blocks_needed = self.config.l1_safe_lag,
            l2_blocks_needed = 1,
            "waiting for block fetch after resync"
        );

        // Fetch latest L2 block
        let max_retries_l2_fetch = 20;
        let num_l2_blocks_to_fetch = 5;
        let latest_blocks = self.fetch_latest_n_blocks_with_retry(max_retries_l2_fetch, num_l2_blocks_to_fetch).map_err(|e| {
            error!("failed to fetch latest L2 block during resync: {}", e);
            e
        });
        if let Ok(mut latest_blocks) = latest_blocks {
            latest_blocks.sort_by_key(|block| block.number);
            for block in latest_blocks {
                self.ctx.new_l2_block(&block);
            }
        } else {
            panic!("failed to fetch latest L2 block during resync, retried {} times", max_retries_l2_fetch);
        }
        
        // Wait for enough L1 blocks to be received
        while !self.is_ready() {
            self.recv_blocks();
            std::thread::sleep(Duration::from_millis(250));
        }

        info!(
            new_state = ?self.ctx,
            "resync complete, refreshing anchor"
        );
        
        // Refresh anchor with new block data
        self.maybe_refresh_anchor();

        info!(
            new_state_after_anchor_refresh = ?self.ctx,
            "resync complete, anchor refreshed"
        );
    }

    fn fetch_latest_n_blocks_with_retry(&self, max_retries: u32, num_blocks: u64) -> eyre::Result<Vec<Header>> {
        let l2_rpc_url = self.l2_config.rpc_url.clone();
        let mut attempts = 0;
        
        loop {
            match fetch_latest_n_blocks_sync(l2_rpc_url.clone(), num_blocks) {
                Ok(blocks) => return Ok(blocks),
                Err(e) => {
                    attempts += 1;
                    if attempts >= max_retries {
                        return Err(e);
                    }
                    // Exponential backoff: 100ms, 200ms, 400ms, 800ms, etc.
                    sleep(Duration::from_millis(100 * 2_u64.pow(attempts - 1)));
                }
            }
        }
    }
}

// https://github.com/taikoxyz/taiko-mono/blob/68cb4367e07ee3fc60c2e09a9eee718c6e45af97/packages/protocol/contracts/L1/libs/LibProposing.sol#L127
fn get_block_env_from_anchor(
    block_number: u64,
    max_gas_limit: u64,
    coinbase: Address,
    timestamp: u64,
    base_fee: u128,
) -> BlockEnv {
    let gas_limit = max_gas_limit + ANCHOR_GAS_LIMIT;
    let difficulty = get_difficulty(block_number);

    BlockEnv {
        number: U256::from(block_number),
        coinbase,
        timestamp: U256::from(timestamp),
        gas_limit: U256::from(gas_limit),
        basefee: U256::from(base_fee),
        difficulty: difficulty.into(),
        prevrandao: Some(difficulty),
        blob_excess_gas_and_price: None,
    }
}
