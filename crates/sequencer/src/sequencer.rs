//! Manages communication with simulator and sequence incoming transactions

use std::{
    sync::Arc,
    thread::sleep,
    time::{Duration, Instant},
};

use alloy_consensus::Transaction;
use alloy_primitives::{utils::format_ether, Address, U256};
use alloy_rpc_types::{Block, Header};
use alloy_signer_local::PrivateKeySigner;
use crossbeam_channel::Receiver;
use eyre::bail;
use pc_common::{
    config::{SequencerConfig, TaikoChainParams, TaikoConfig},
    proposer::{is_propose_delayed, ProposalRequest},
    runtime::spawn,
    sequencer::{ExecutionResult, Order, StateId},
    taiko::{
        get_difficulty, get_extra_data, lookahead::LookaheadHandle, pacaya::BlockParams,
        AnchorParams, ANCHOR_GAS_LIMIT, GOLDEN_TOUCH_ADDRESS,
    },
    types::BlockEnv,
};
use reqwest::Client;
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, warn, Instrument};

use crate::{
    context::{SequencerContext, SequencerState},
    simulator::SimulatorClient,
    soft_block::BuildPreconfBlockRequestBody,
    tx_pool::TxPool,
};

pub struct SequencerSpine {
    /// Receive txs and bundles from RPC
    pub rpc_rx: Receiver<Order>,
    /// Send blocks to proposer for inclusion
    pub proposer_tx: UnboundedSender<ProposalRequest>,
    // Receiver of L1 blocks
    pub l1_blocks_rx: Receiver<Header>,
    // Receiver of L2 non preconf blocks
    pub l2_blocks_rx: Receiver<Header>,
}

pub struct Sequencer {
    config: SequencerConfig,
    chain_config: TaikoChainParams,
    ctx: SequencerContext,
    simulator: SimulatorClient,
    tx_pool: TxPool,
    spine: SequencerSpine,
    lookahead: LookaheadHandle,
    can_sequence_lookahead: bool,
    signer: PrivateKeySigner,
    proposer_request: Option<ProposalRequest>,
}

impl Sequencer {
    pub fn new(
        config: SequencerConfig,
        taiko_config: TaikoConfig,
        mempool_rx: Receiver<Order>,
        spine: SequencerSpine,
        lookahead: LookaheadHandle,
        signer: PrivateKeySigner,
    ) -> Self {
        let chain_config = taiko_config.params;
        let simulator = SimulatorClient::new(config.simulator_url.clone(), taiko_config);

        // this doesn't handle well the restarts if we had pending orders in the rpc
        let ctx = SequencerContext::new(config.l1_safe_lag);
        let tx_pool = TxPool::new(mempool_rx);

        Self {
            config,
            chain_config,
            ctx,
            simulator,
            tx_pool,
            spine,
            lookahead,
            signer,
            can_sequence_lookahead: false,
            proposer_request: None,
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
            sleep(Duration::from_millis(250));
        }

        info!("starting loop");

        loop {
            // fetch new data
            self.recv_blocks();
            self.tx_pool.fetch();
            self.maybe_refresh_anchor();

            // clear proposal if it's late
            if self.lookahead.should_clear_proposal(&self.signer.address()) {
                self.send_request_to_proposer();
            }

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

                    let should_seal = seal_by_time || seal_by_gas;

                    if should_seal || !self.can_sequence() {
                        if let Err(err) = self.commit_seal(tip_state_id, start, anchor_params) {
                            // todo: add a failsafe so we're not stuck forever here
                            error!(%err, "failed commit seal");
                            panic!("failed commit seal");
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

    fn can_sequence(&mut self) -> bool {
        if is_propose_delayed() {
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

        let (can_sequence, reason) = self.lookahead.can_sequence(&self.signer.address());

        if can_sequence {
            if !self.can_sequence_lookahead {
                warn!("can now sequence based on lookahead: {reason}");
                self.can_sequence_lookahead = true;
            }
        } else if self.can_sequence_lookahead {
            warn!("can no longer sequence based on lookahead: {reason}");
            self.can_sequence_lookahead = false;
        }

        !self.ctx.l1_delayed && self.can_sequence_lookahead
    }

    // TODO: if sim fails, rpc orders are lost
    fn next_order(&mut self) -> Option<Order> {
        self.spine.rpc_rx.try_recv().ok().or(self.tx_pool.next_sequence())
    }

    fn recv_blocks(&mut self) {
        if let Ok(new_header) = self.spine.l1_blocks_rx.try_recv() {
            self.ctx.new_l1_block(new_header);
        }

        if let Ok(new_header) = self.spine.l2_blocks_rx.try_recv() {
            self.ctx.new_l2_block(&new_header);
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
        let is_anchor_old =
            self.ctx.current_anchor_id() + self.config.anchor_batch_lag < safe_l1_header.number;

        // if the blocks with this anchor has too many txs, refresh it
        let is_batch_big = false; // TODO

        if is_anchor_old || is_batch_big {
            let new = AnchorParams {
                block_id: safe_l1_header.number,
                state_root: safe_l1_header.state_root,
                timestamp: self.ctx.parent.timestamp.max(safe_l1_header.timestamp),
            };

            self.ctx.anchor = new;

            debug!(anchor =? self.ctx.anchor, "refreshed anchor params");

            self.send_request_to_proposer();
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
        self.tx_pool.clear_mined(txs);

        self.ctx.new_preconf_l2_block(&block.header);
        self.gossip_soft_block(block.clone());

        // self.send_request_to_proposer();

        let is_first_block = self.proposer_request.is_none();
        let request = &mut self.proposer_request.get_or_insert(ProposalRequest::default());

        if is_first_block {
            request.anchor_block_id = anchor_params.block_id;
            request.start_block_num = block_number;
        } else {
            assert_eq!(request.anchor_block_id, anchor_params.block_id);
        }

        request.end_block_num = block_number;
        request.last_timestamp = block.header.timestamp;
        request.block_params.push(BlockParams {
            numTransactions: (block.transactions.len() - 1) as u16, // exclude anchor tx
            timeShift: 0,
            signalSlots: vec![],
        });
        let txs = Arc::unwrap_or_clone(block)
            .transactions
            .into_transactions()
            .filter(|tx| tx.from != GOLDEN_TOUCH_ADDRESS)
            .map(|tx| tx.inner);
        request.all_tx_list.extend(txs);

        Ok(())
    }

    fn send_request_to_proposer(&mut self) {
        if let Some(request) = std::mem::take(&mut self.proposer_request) {
            let _ = self.spine.proposer_tx.send(request);
        }
    }

    #[tracing::instrument(skip_all, name = "soft_blocks", fields(block = block.header.number))]
    fn gossip_soft_block(&self, block: Arc<Block>) {
        debug!(block_hash = %block.header.hash, "gossiping soft block");
        let signer = self.signer.clone();

        let url = self.config.soft_block_url.clone();

        spawn(
            async move {
                let request = BuildPreconfBlockRequestBody::new(block, signer).await.unwrap();

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
