//! Manages communication with simulator and sequence incoming transactions

use std::{
    sync::{atomic::AtomicU64, Arc},
    thread::sleep,
    time::{Duration, Instant},
};

use alloy_consensus::Transaction;
use alloy_primitives::{utils::format_ether, Address, U256};
use alloy_rpc_types::{Block, Header};
use crossbeam_channel::{Receiver, Sender};
use eyre::bail;
use pc_common::{
    config::{SequencerConfig, TaikoChainParams, TaikoConfig},
    metrics::{BlocksMetrics, SequencerMetrics},
    proposer::{is_propose_delayed, ProposalRequest, ProposeBatchParams, TARGET_BATCH_SIZE},
    sequencer::{ExecutionResult, StateId},
    taiko::{
        get_difficulty, get_extra_data,
        lookahead::LookaheadHandle,
        pacaya::{estimate_compressed_size, BlockParams},
        AnchorParams, ANCHOR_GAS_LIMIT,
    },
    types::BlockEnv,
    utils::utcnow_sec,
};
use reqwest::{header::AUTHORIZATION, Client};
use tracing::{debug, error, info, warn, Instrument};

use crate::{
    context::SequencerContext,
    error::SequencerError,
    jwt::generate_jwt,
    simulator::SimulatorClient,
    soft_block::BuildPreconfBlockRequestBody,
    sorting::SortData,
    tx_pool::TxPool,
    types::{BlockInfo, SequencerSpine, SequencerState, SimulatedOrder},
};

#[derive(Debug, Default)]
struct SequencerFlags {
    /// Can we sequence new blocks
    can_sequence: bool,
    /// Can we propose new batches
    can_propose: bool,
    /// Is block proposing delayed
    proposing_delayed: bool,
    /// Is the L1 block fetch delayed
    l1_delayed: bool,
    /// Is it our turn to sequence based on the lookahead
    lookahead_sequence: bool,
}

pub struct Sequencer {
    config: SequencerConfig,
    chain_config: TaikoChainParams,
    ctx: SequencerContext,
    simulator: SimulatorClient,
    tx_pool: TxPool,
    spine: SequencerSpine,
    lookahead: LookaheadHandle,
    flags: SequencerFlags,
    proposer_request: Option<ProposeBatchParams>,
    last_anchor_error: Instant,
}

impl Sequencer {
    pub fn new(
        config: SequencerConfig,
        taiko_config: TaikoConfig,
        spine: SequencerSpine,
        lookahead: LookaheadHandle,
        l2_origin: Arc<AtomicU64>,
        l1_number: Arc<AtomicU64>,
        sim_tx: Sender<eyre::Result<SimulatedOrder>>,
    ) -> Self {
        let chain_config = taiko_config.params;
        let simulator = SimulatorClient::new(config.simulator_url.clone(), taiko_config, sim_tx);
        let ctx = SequencerContext::new(
            config.l1_safe_lag,
            l2_origin,
            l1_number,
            config.coinbase_address,
        );

        Self {
            config,
            chain_config,
            ctx,
            simulator,
            tx_pool: TxPool::new(),
            spine,
            lookahead,
            flags: SequencerFlags::default(),
            proposer_request: None,
            last_anchor_error: Instant::now(),
        }
    }

    /// Main sequencer loop:
    /// - we send batches for proposal when we have a new anchor or if we're approaching the next
    ///   operator in the lookahead
    #[tracing::instrument(skip_all, name = "sequencer")]
    pub fn run(mut self) {
        info!("waiting for block fetch");

        while !self.is_ready() {
            self.recv_blocks();
            sleep(Duration::from_millis(50));
        }

        info!("starting loop");

        loop {
            self.tick();

            let state = std::mem::take(&mut self.ctx.state);
            self.ctx.state = self.state_transition(state);

            self.record_metrics();
        }
    }

    /// Periodic updates done at every loop
    fn tick(&mut self) {
        // fetch new data
        self.recv_blocks();
        self.fetch_txs();

        // handle sim results
        self.handle_sims();

        // check capabilities
        self.check_can_sequence();
        self.check_can_propose();

        // refresh anchor if needed
        if self.flags.can_sequence {
            self.maybe_refresh_anchor();
        }

        // clear proposal if it's late
        if self.lookahead.should_clear_proposal(&self.config.operator_address).0 {
            self.send_batch_to_proposer("approaching next operator", true);
        }

        // clear proposal if time shift is too long
        if self.check_time_shift() {
            self.send_batch_to_proposer("time shift too long", true);
        }
    }

    fn state_transition(&mut self, state: SequencerState) -> SequencerState {
        match state {
            SequencerState::Sync { last_l1 } => {
                let Some(safe_l1_header) = self.ctx.safe_l1_header() else {
                    return state;
                };

                let safe_l1 = safe_l1_header.number;
                if safe_l1 < self.ctx.parent_anchor_block_id {
                    // need to wait for L1 blocks, is the safe lag too large?
                    if safe_l1 != last_l1 {
                        warn!(
                            safe_l1,
                            parent_anchor_block = self.ctx.parent_anchor_block_id,
                            "waiting for safe L1 block (this should be rare)"
                        );
                    }

                    return SequencerState::Sync { last_l1: safe_l1 };
                }

                // after here we have enough L1 blocks, dont need to check it anymore
                if !self.flags.can_sequence {
                    return SequencerState::default();
                }

                let max_block_size = TARGET_BATCH_SIZE.saturating_sub(
                    self.proposer_request.as_ref().map(|p| p.compressed_est).unwrap_or(0),
                );

                if max_block_size == 0 {
                    return SequencerState::default();
                }

                if !self.tx_pool.new_orders() {
                    return SequencerState::default();
                }

                // because block time is >> 100ms we dont need to reset this if anchor succeeds
                // ie. we never need to anchor more frequently than every 100ms
                if self.last_anchor_error.elapsed() < Duration::from_millis(100) {
                    return SequencerState::default();
                }

                match self.anchor_block() {
                    Ok((state_id, block_info)) => {
                        debug!(?block_info, %state_id, "anchored");
                        let Some(active) = self
                            .tx_pool
                            .active_orders(block_info.block_number, block_info.base_fee)
                        else {
                            self.tx_pool.set_no_orders();
                            return SequencerState::default();
                        };

                        // we have orders, start building a block
                        info!(
                            block = block_info.block_number,
                            txs = active.active_txs(),
                            senders = active.active_senders(),
                            target_time = ?self.config.target_block_time,
                            max_block_size,
                            gas_limit = self.chain_config.block_max_gas_limit,
                            "start block building"
                        );

                        let sort_data = SortData::new(
                            block_info,
                            state_id,
                            active,
                            self.chain_config.block_max_gas_limit,
                            self.config.target_block_time,
                            max_block_size,
                        );

                        SequencerState::Sorting(sort_data)
                    }

                    Err(err) => {
                        error!(%err, "failed anchoring");
                        self.last_anchor_error = Instant::now();
                        SequencerState::default()
                    }
                }
            }

            SequencerState::Sorting(sort_data) if sort_data.should_discard() => {
                error!("invalid state detected, resetting sorting loop");
                self.tx_pool.clear_nonces();
                if self.needs_anchor_refresh(&sort_data.block_info.anchor_params) {
                    self.send_batch_to_proposer(
                        "sealed last for this anchor (invalid state detected)",
                        false,
                    );
                }
                SequencerState::default()
            }

            SequencerState::Sorting(sort_data) if sort_data.should_seal() => {
                if sort_data.num_txs() > 0 {
                    if let Err(err) = self.seal_block(sort_data) {
                        if let SequencerError::SoftBlock(status, err) = err {
                            warn!(status, %err, "failed commit seal");
                            SequencerState::default()
                        } else {
                            error!(%err, "failed commit seal");
                            panic!("failed commit seal");
                        }
                    } else {
                        // reset state for next block
                        SequencerState::default()
                    }
                } else {
                    // if we're here, all the orders were invalid, so the state nonces are
                    // all for the actual state db (as opposed to ones we applied in the block),
                    // use those to clear the txpool and restart the loop
                    self.tx_pool.update_nonces(sort_data.state_nonces);
                    debug!("exhausted active orders past target seal, resetting");
                    if self.needs_anchor_refresh(&sort_data.block_info.anchor_params) {
                        self.send_batch_to_proposer(
                            "sealed last for this anchor (no active orders)",
                            false,
                        );
                    }
                    SequencerState::default()
                }
            }

            SequencerState::Sorting(mut sort_data) => {
                sort_data.maybe_sim_next_batch(&self.simulator, self.config.max_sims_per_loop);

                if sort_data.is_simulating() {
                    return SequencerState::Sorting(sort_data);
                }

                // if we're not simulating here then we have run out of active orders
                // try to get new orders from the txpool considering the state nonces we have
                if let Some(new_active) = self
                    .tx_pool
                    .get_active_for_nonces(&sort_data.state_nonces, sort_data.block_info.base_fee)
                {
                    sort_data.active_orders = new_active;
                    SequencerState::Sorting(sort_data)
                } else {
                    SequencerState::Sorting(sort_data)
                }
            }
        }
    }

    /// We can sequence if:
    /// - batch proposal is not delayed
    /// - L1 block fetch is not delayed
    /// - it's our turn to sequence based on the lookahead
    fn check_can_sequence(&mut self) {
        let is_propose_delayed = is_propose_delayed();
        if is_propose_delayed != self.flags.proposing_delayed {
            if is_propose_delayed {
                warn!("block proposing is delayed, stop sequencing");
            } else {
                warn!("block proposing has resumed");
            }
            self.flags.proposing_delayed = is_propose_delayed;
        }

        let is_l1_delayed = self.ctx.last_l1_receive.elapsed() > self.config.l1_delay;
        if is_l1_delayed != self.flags.l1_delayed {
            if is_l1_delayed {
                warn!(last_received = ?self.ctx.last_l1_receive.elapsed(), "l1 block fetch is delayed");
            } else {
                warn!("l1 block fetch has resumed");
            }
            self.flags.l1_delayed = is_l1_delayed;
        }

        let (can_sequence, reason) = self.lookahead.can_sequence(&self.config.operator_address);
        if can_sequence != self.flags.lookahead_sequence {
            if can_sequence {
                warn!(reason, "can now sequence based on lookahead");
            } else {
                warn!(reason, "can no longer sequence based on lookahead");
            }
            self.flags.lookahead_sequence = can_sequence;
        }

        self.flags.can_sequence = !self.flags.proposing_delayed &&
            !self.flags.l1_delayed &&
            self.flags.lookahead_sequence;
    }

    fn check_can_propose(&mut self) {
        let (can_propose, reason) = self.lookahead.can_propose(&self.config.operator_address);

        if can_propose != self.flags.can_propose {
            if can_propose {
                warn!(reason, "can now propose");
                self.check_resync();
            } else {
                warn!(reason, "can no longer propose");
            }
            self.flags.can_propose = can_propose;
        }
    }

    // if some blocks havent landed yet, we resync to the latest anchor
    fn check_resync(&mut self) {
        let origin = self.ctx.l2_origin();
        let target = self
            .proposer_request
            .as_ref()
            .map(|p| p.start_block_num - 1) // assumption is that we're still in the first batch being sequenced
            .unwrap_or(self.ctx.l2_parent().block_number);

        if origin < target {
            warn!(origin, target, "resyncing to L2 origin");
            let _ = self.spine.proposer_tx.send(ProposalRequest::Resync { origin, end: target });
        }
    }

    fn check_time_shift(&self) -> bool {
        // timeShift is at most 255 secs
        self.proposer_request
            .as_ref()
            .map(|p| utcnow_sec().saturating_sub(p.last_timestamp) > 230)
            .unwrap_or(false)
    }

    fn recv_blocks(&mut self) {
        receive_for(
            Duration::from_millis(10),
            |new_header| {
                self.ctx.new_l1_block(new_header);
            },
            &self.spine.l1_blocks_rx,
        );

        receive_for(
            Duration::from_millis(10),
            |new_block| {
                self.ctx.new_preconf_l2_block(&new_block);
            },
            &self.spine.l2_blocks_rx,
        );

        receive_for(
            Duration::from_millis(10),
            |new_header| {
                self.ctx.new_origin_l2_block(&new_header);
            },
            &self.spine.origin_blocks_rx,
        );
    }

    fn fetch_txs(&mut self) {
        receive_for(
            Duration::from_millis(10),
            |tx| {
                self.tx_pool.put(tx, self.ctx.l2_parent().block_number);
            },
            &self.spine.rpc_rx,
        );

        receive_for(
            Duration::from_millis(10),
            |tx| {
                self.tx_pool.put(tx, self.ctx.l2_parent().block_number);
            },
            &self.spine.mempool_rx,
        );
    }

    fn handle_sims(&mut self) {
        if let Ok(sim_res) = self.spine.sim_rx.try_recv() {
            let sim_res = match sim_res {
                Ok(sim_res) => sim_res,
                Err(err) => {
                    // this is when simulator is down, maybe just stop sequencing
                    let msg = format!("failed sim: {}", err);
                    error!("{msg}");
                    panic!("{msg}");
                }
            };

            let SequencerState::Sorting(sort_data) = &mut self.ctx.state else {
                warn!(sim_tx = %sim_res.order.tx_hash(), origin_state_id =% sim_res.origin_state_id, current_state =? self.ctx.state, "stale sim result");
                return;
            };

            sort_data.handle_sim(sim_res);
        }
    }

    fn is_ready(&self) -> bool {
        self.ctx.l1_headers.len() as u64 >= self.config.l1_safe_lag &&
            self.ctx.l2_headers.back().is_some()
    }

    fn maybe_refresh_anchor(&mut self) {
        let Some(safe_l1_header) = self.ctx.safe_l1_header().cloned() else {
            error!("missing l1 headers");
            return;
        };

        let Some(ctx_anchor) = self.ctx.anchor.as_ref() else {
            self.update_anchor(&safe_l1_header, "no anchor");
            return;
        };

        // if current anchor has been used for enough blocks, refresh it
        let is_anchor_old =
            ctx_anchor.block_id + self.config.anchor_batch_lag < safe_l1_header.number;

        if is_anchor_old {
            self.update_anchor(&safe_l1_header, "anchor too old");
            return;
        }

        // if the batch with this anchor has too many txs, refresh it
        let current_batch_size =
            self.proposer_request.as_ref().map(|p| p.compressed_est).unwrap_or(0);
        // note we could exceed the size here if the last block is large, but we have a buffer so
        // shouldnt be a problem
        let is_batch_big = current_batch_size > TARGET_BATCH_SIZE;

        if is_batch_big {
            self.update_anchor(&safe_l1_header, "batch too big");
        }
    }

    fn update_anchor(&mut self, safe_l1_header: &Header, reason: &str) {
        let new = AnchorParams {
            block_id: safe_l1_header.number,
            state_root: safe_l1_header.state_root,
            timestamp: self.ctx.l2_parent().timestamp.max(safe_l1_header.timestamp),
        };

        debug!(reason, anchor =? new, "refresh anchor");
        self.ctx.anchor = Some(new);
        self.send_batch_to_proposer(reason, true);
    }

    /// Anchors the current block
    fn anchor_block(&self) -> eyre::Result<(StateId, BlockInfo)> {
        let Some(anchor) = self.ctx.anchor else {
            bail!("no anchor");
        };

        let parent = *self.ctx.l2_parent();

        debug!(?anchor, parent =? parent, "assembling anchor");

        let timestamp = utcnow_sec() + self.config.target_block_time.as_secs();
        let (tx, l2_base_fee) = self.simulator.assemble_anchor(timestamp, parent, anchor)?;

        let block_number = parent.block_number + 1;
        let block_env = get_block_env_from_anchor(
            block_number,
            self.chain_config.block_max_gas_limit,
            self.config.coinbase_address,
            timestamp,
            l2_base_fee,
        );
        let block_info = BlockInfo { anchor_params: anchor, block_number, base_fee: l2_base_fee };

        let extra_data = get_extra_data(self.chain_config.base_fee_config.sharing_pctg);

        let start = Instant::now();
        match self.simulator.simulate_anchor(tx, block_env, extra_data) {
            Ok(res) => {
                let ExecutionResult::Success { state_id, gas_used, builder_payment } = res else {
                    bail!("failed simulate anchor, res={res:?}");
                };

                debug!(sim_time =? start.elapsed(), anchor = ?self.ctx.anchor, ?parent, gas_used, builder_payment, "simulated anchor");
                Ok((state_id, block_info))
            }
            Err(err) => {
                bail!("failed simulate anchor, err={err}")
            }
        }
    }

    fn seal_block(&mut self, sort_data: SortData) -> Result<(), SequencerError> {
        sort_data.report();

        let seal_state_id = sort_data.state_id;
        let start_block = sort_data.start_build;
        let anchor_params = sort_data.block_info.anchor_params;

        // seal
        let start = Instant::now();
        let res = self.simulator.seal_block(seal_state_id)?;
        let seal_time = start.elapsed();

        let block = res.built_block;
        let block_number = block.header.number;
        let block_time = start_block.elapsed();

        info!(
            bn = block_number,
            ?seal_time,
            ?block_time,
            block_hash = %block.header.hash,
            payment = format_ether(res.cumulative_builder_payment),
            gas_used = res.cumulative_gas_used,
            "sealed block"
        );

        // fail if gossiping fails
        self.gossip_soft_block(&block)?;

        BlocksMetrics::built_block(block_time, res.cumulative_builder_payment);

        let txs = block.transactions.txns().map(|tx| (tx.from, tx.nonce()));
        self.tx_pool.clear_mined(block_number, txs);

        self.ctx.new_preconf_l2_block(&block);

        let is_first_block = self.proposer_request.is_none();
        let request = &mut self.proposer_request.get_or_insert(ProposeBatchParams::default());

        let time_shift;
        if is_first_block {
            request.anchor_block_id = anchor_params.block_id;
            request.start_block_num = block_number;
            request.coinbase = self.config.coinbase_address;
            time_shift = 0;
        } else {
            assert_eq!(request.anchor_block_id, anchor_params.block_id);
            time_shift = block
                .header
                .timestamp
                .saturating_sub(request.last_timestamp)
                .try_into()
                .expect("exceeed u8 time shift");
        }

        request.end_block_num = block_number;
        request.last_timestamp = block.header.timestamp;
        request.block_params.push(BlockParams {
            numTransactions: (block.transactions.len() - 1) as u16, // exclude anchor tx
            timeShift: time_shift,
            signalSlots: vec![],
        });

        assert_eq!(
            block.transactions.len(), // with anchor
            sort_data.orders.len() + 1,
            "mismatch in tx from block and sorting"
        );

        request.all_tx_list.extend(sort_data.orders);
        request.compressed_est += estimate_compressed_size(sort_data.uncompressed_size);

        info!(
            start = request.start_block_num,
            end = request.end_block_num,
            est_batch_size = request.compressed_est,
            txs = request.all_tx_list.len(),
            "batch info"
        );

        if self.needs_anchor_refresh(&anchor_params) {
            self.send_batch_to_proposer("sealed last for this anchor", false);
        }

        Ok(())
    }

    fn needs_anchor_refresh(&self, anchor_params: &AnchorParams) -> bool {
        self.ctx.anchor.map(|a| a.block_id != anchor_params.block_id).unwrap_or(true)
    }

    /// Triggered:
    /// - if we're approaching the end of this operator's turn
    /// - if we seal a block and next anchor will be different
    /// - if we refresh the anchor and have some pending from the previous anchor
    fn send_batch_to_proposer(&mut self, reason: &str, state_check: bool) {
        if state_check {
            if let SequencerState::Sorting(sort_data) = &mut self.ctx.state {
                // avoid breaking the batch while sorting
                // this will be sent after sealing the current block and we'll seal at next loop
                sort_data.set_should_seal();
                return;
            }
        }

        if let Some(request) = std::mem::take(&mut self.proposer_request) {
            warn!(reason, "sending batch to be proposed");
            let _ = self.spine.proposer_tx.send(ProposalRequest::Batch(request));
        }
    }

    fn gossip_soft_block(&self, block: &Block) -> Result<(), SequencerError> {
        debug!(block_hash = %block.header.hash, "gossiping soft block");
        let url = self.config.soft_block_url.clone();

        let jwt_secret = self.config.jwt_secret.clone();

        self.simulator.block_on(
            async move {
                let block_number = block.header.number;
                let request = BuildPreconfBlockRequestBody::new(block);

                let mut req_builder = Client::new().post(url).json(&request);

                if !jwt_secret.is_empty() {
                    let jwt = generate_jwt(jwt_secret).unwrap();
                    req_builder = req_builder.header(AUTHORIZATION, format!("Bearer {}", jwt));
                }

                let res = req_builder.send().await?;

                let status = res.status();
                let body = res.text().await.unwrap();

                if status.is_success() {
                    debug!(block_number, "soft block posted");
                    Ok(())
                } else {
                    Err(SequencerError::SoftBlock(status.as_u16(), body))
                }
            }
            .in_current_span(),
        )
    }

    fn record_metrics(&self) {
        self.ctx.state.record_metrics();

        SequencerMetrics::set_is_sequencer(self.flags.can_sequence);
        SequencerMetrics::set_is_proposer(self.flags.can_propose);
    }
}

// https://github.com/taikoxyz/taiko-mono/blob/68cb4367e07ee3fc60c2e09a9eee718c6e45af97/packages/protocol/contracts/L1/libs/LibProposing.sol#L127
fn get_block_env_from_anchor(
    block_number: u64,
    max_gas_limit: u128,
    coinbase: Address,
    timestamp: u64,
    base_fee: u128,
) -> BlockEnv {
    let gas_limit = max_gas_limit + ANCHOR_GAS_LIMIT as u128;
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

/// Receives messages from a channel for a limited duration
fn receive_for<T, F>(duration: Duration, mut handler: F, rx: &Receiver<T>)
where
    F: FnMut(T),
{
    let start = Instant::now();
    while let Ok(item) = rx.try_recv() {
        handler(item);
        if start.elapsed() > duration {
            break;
        }
    }
}
