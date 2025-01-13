//! Manages communication with simulator and sequence incoming transactions

use std::{mem, sync::Arc, time::Instant};

use alloy_consensus::{Transaction, TxEnvelope};
use alloy_primitives::{utils::format_ether, Address, U256};
use alloy_rpc_types::Block;
use crossbeam_channel::Receiver;
use pc_common::{
    config::{SequencerConfig, TaikoChainConfig},
    driver::{DriverRequest, DriverResponse},
    proposer::ProposerRequest,
    sequencer::{Order, SequenceLoop, StateId},
    taiko::{get_difficulty, get_extra_data, ANCHOR_GAS_LIMIT},
    types::{AnchorData, BlockEnv, DuplexChannel},
};
use tracing::{debug, error, info, warn};

use crate::{
    context::SequencerContext,
    txpool::TxPool,
    types::{WorkerError, WorkerRequest, WorkerResponse},
};

// TODO: abstract to multiple strategies
pub struct Sequencer {
    config: SequencerConfig,
    chain_config: TaikoChainConfig,
    context: SequencerContext,
    /// Receive txs and bundles from RPC
    rpc_rx: Receiver<Order>,
    /// Mempool transactions
    txpool: TxPool,
    /// Send blocks to proposer for inclusion and receive loop signals
    to_proposer: DuplexChannel<ProposerRequest, SequenceLoop>,
    /// Send worker requests and receive simulations
    to_worker: DuplexChannel<WorkerRequest, Result<WorkerResponse, WorkerError>>,
    /// Send anchor tx request and receive anchors
    to_driver: DuplexChannel<DriverRequest, DriverResponse>,
}

impl Sequencer {
    pub fn new(
        config: SequencerConfig,
        chain_config: TaikoChainConfig,
        rpc_rx: Receiver<Order>,
        mempool_rx: Receiver<Arc<TxEnvelope>>,
        to_proposer: DuplexChannel<ProposerRequest, SequenceLoop>,
        to_worker: DuplexChannel<WorkerRequest, Result<WorkerResponse, WorkerError>>,
        to_driver: DuplexChannel<DriverRequest, DriverResponse>,
    ) -> Self {
        // this doesn't handle well the restarts if we had pending orders in the rpc
        let context = SequencerContext::new();
        let txpool = TxPool::new(mempool_rx);
        Self { config, chain_config, context, rpc_rx, txpool, to_proposer, to_worker, to_driver }
    }

    #[tracing::instrument(skip_all, name = "sequencer")]
    pub fn run(mut self) {
        loop {
            self.sequence_next();

            self.txpool.try_fetch_txs();

            if let Ok(reponse) = self.to_driver.rx.try_recv() {
                self.handle_driver_response(reponse)
            }

            if let Ok(response) = self.to_worker.rx.try_recv() {
                self.handle_worker_response(response);
            }

            if let Ok(signal) = self.to_proposer.rx.try_recv() {
                self.handle_loop_signal(signal)
            }
        }
    }

    ///////////////// WORKER /////////////////

    fn send_worker_request(&self, request: WorkerRequest) {
        if let Err(err) = self.to_worker.tx.send(request) {
            error!(%err, "failed sequencer->worker");
        };
    }

    fn send_simulate(&mut self, state_id: StateId, order: Order) {
        self.context.is_simulating = true;
        self.send_worker_request(WorkerRequest::Simulate { state_id, order });
    }
    fn send_simulate_anchor(&mut self, tx: TxEnvelope, anchor_data: AnchorData) {
        self.context.is_simulating = true;

        // anchors are always for next block, assuming we build one block at a time
        let block_number = self.context.head_block_number + 1;

        let block_env = get_block_env_from_anchor(
            block_number,
            self.chain_config.block_max_gas_limit,
            self.config.coinbase_address,
            &anchor_data,
        );

        let block_env = Box::new(block_env);
        let extra_data = get_extra_data(self.chain_config.base_fee_config.sharingPctg);
        self.send_worker_request(WorkerRequest::Anchor { tx, anchor_data, block_env, extra_data });
    }
    fn send_commit(&mut self, state_id: StateId) {
        self.context.is_simulating = true;
        self.send_worker_request(WorkerRequest::Commit { state_id });
    }
    fn send_seal(&mut self) {
        self.context.is_simulating = true;
        self.send_worker_request(WorkerRequest::Seal);
    }
    // this only works for now since we do one sim at a time
    fn reset_state(&mut self) {
        self.context.is_simulating = false;
    }

    fn handle_worker_response(&mut self, response: Result<WorkerResponse, WorkerError>) {
        self.reset_state();

        let response = match response {
            Ok(response) => response,
            Err(err) => {
                error!(%err, "failed simulation");
                return;
            }
        };

        match response {
            // super simple one simulation/commit/seal at a time
            // because of this we dont need to re-send for simulation if a tx was simulated on the
            // wrong state, ie. it's not possible we replace the anchor env while a user
            // tx is in flight
            WorkerResponse::Simulate {
                origin_state_id,
                new_state_id,
                gas_used,
                builder_payment,
            } => {
                if let Err(err) = self.context.sequence_tx(
                    origin_state_id,
                    new_state_id,
                    gas_used,
                    builder_payment,
                ) {
                    error!(%err, "failed to sequence transaction");
                } else {
                    self.send_commit(new_state_id);
                }
            }

            WorkerResponse::Anchor { env: new_env } => {
                match self.context.anchor_env.as_mut() {
                    Some(anchor_env) => {
                        if anchor_env.is_building() {
                            debug!(
                                bn = anchor_env.block_number,
                                old_block_id = anchor_env.data.block_id,
                                new_block_id = new_env.data.block_id,
                                "building block, discarding new anchor env"
                            );
                        } else if anchor_env.is_older_anchor(&new_env) {
                            debug!(
                                bn = new_env.block_number,
                                old_block_id = anchor_env.data.block_id,
                                new_block_id = new_env.data.block_id,
                                "replacing anchor env"
                            );

                            // refresh anchor env if we're not in middle of a block
                            self.context.anchor_env = Some(new_env);
                        }
                    }

                    None => {
                        debug!(
                            bn = new_env.block_number,
                            block_id = new_env.data.block_id,
                            "new anchor env"
                        );

                        self.context.anchor_env = Some(new_env);
                    }
                }
            }

            WorkerResponse::Commit { commit, origin_state_id } => {
                info!(
                    committed = %origin_state_id,
                    gas_used = commit.cumulative_gas_used,
                    payment = format_ether(commit.cumulative_builder_payment),
                    "committed state"
                );
            }

            WorkerResponse::Block(block) => {
                let block_number = block.built_block.header.number;

                let anchor_env =
                    mem::take(&mut self.context.anchor_env).expect("sealed with no env");

                assert_eq!(anchor_env.block_number, block_number, "block number mismatch");

                self.context.head_block_number = block_number;

                let first_sequenced = anchor_env
                    .first_sequenced
                    .expect("first sequenced was not set. Did you sequence only the anchor?");

                info!(
                    bn = block_number,
                    block_hash = %block.built_block.header.hash,
                    block_time = ?first_sequenced.elapsed(),
                    payment = format_ether(block.cumulative_builder_payment),
                    "sealed block"
                );

                self.notify_new_block(block.built_block.clone());

                let txs = block.built_block.transactions.txns().map(|tx| (tx.from, tx.nonce()));
                self.txpool.clear_mined(txs);

                if !self.config.dry_run {
                    self.send_block_to_proposer(
                        block.built_block,
                        anchor_env.data.block_id,
                        anchor_env.data.timestamp,
                    );
                }
            }
        }
    }

    // TODO: remove this
    fn skip_sequence(&self) -> bool {
        self.context.is_simulating ||
            self.context.stop_by < Instant::now() ||
            self.context.head_block_number == 0
    }

    /// Send next order for simulation or anchor the current block
    fn sequence_next(&mut self) {
        if self.skip_sequence() {
            return;
        }

        // do one only action here
        if let Some(anchor_env) = &self.context.anchor_env {
            if let Some(f) = anchor_env.first_sequenced {
                let seal_by_time = f.elapsed() > self.config.target_block_time;
                let seal_by_gas = false;

                if seal_by_time || seal_by_gas {
                    // commit + seal (could be decoupled)
                    info!("sealing {} txs. Block time {:?}", anchor_env.user_txs, f.elapsed());
                    self.send_seal();
                    return;
                }
            }

            if let Ok(order) = self.rpc_rx.try_recv() {
                // we have a live env, try sim on top of it
                self.send_simulate(anchor_env.tip_state_id, order)
            } else if let Some(order) = self.txpool.next_sequence() {
                self.send_simulate(anchor_env.tip_state_id, order)
            }
        } else if let Some((tx, anchor_data)) = mem::take(&mut self.context.anchor_next) {
            if self.context.is_anchor_for_this_block(&tx) {
                // anchor current block
                self.send_simulate_anchor(tx, anchor_data);
            } else {
                // anchor data is stale
                self.refresh_anchor();
            }
        } else {
            // not building a block, missing anchor data, request if user txs are waiting
            if !self.rpc_rx.is_empty() || self.txpool.has_txs() {
                // warn!("missing anchors with user transactions! requesting new anchor");
                self.refresh_anchor();
            }
        }
    }

    ///////////////// PROPOSER /////////////////

    fn send_block_to_proposer(
        &self,
        block: Arc<Block>,
        anchor_block_id: u64,
        anchor_timestamp: u64,
    ) {
        if let Err(err) =
            self.to_proposer.tx.send(ProposerRequest { block, anchor_block_id, anchor_timestamp })
        {
            error!(%err, "failed sequencer->proposer");
        };
    }

    fn handle_loop_signal(&mut self, loop_signal: SequenceLoop) {
        match loop_signal {
            SequenceLoop::Continue { stop_by } => {
                self.context.stop_by = stop_by;
            }
            SequenceLoop::Stop => {
                warn!("STOP sequencer loop");
                self.context.stop_by = Instant::now();
            }
        }
    }

    ///////////////// DRIVER /////////////////

    /// Trigger a new anchor for the next block
    fn notify_new_block(&mut self, block: Arc<Block>) {
        self.context.waiting_for_anchor = true;
        if let Err(err) = self.to_driver.tx.send(DriverRequest::AnchorV2 { block }) {
            error!(%err, "failed sequencer->driver");
        };
    }
    fn refresh_anchor(&mut self) {
        if !self.context.waiting_for_anchor {
            self.context.waiting_for_anchor = true;

            if let Err(err) = self.to_driver.tx.send(DriverRequest::Refresh) {
                error!(%err, "failed sequencer->driver");
            };
        };
    }

    fn handle_driver_response(&mut self, response: DriverResponse) {
        match response {
            DriverResponse::AnchorV2 { tx, anchor_data } => {
                self.context.waiting_for_anchor = false;

                if let Some(anchor_env) = &self.context.anchor_env {
                    // if we're building a block, this anchor is already stale
                    if !anchor_env.is_building() {
                        // refresh anchor env, even if we have one
                        self.send_simulate_anchor(tx, anchor_data);
                    }
                } else {
                    // prepare anchor for next block
                    self.send_simulate_anchor(tx, anchor_data);
                }
            }

            DriverResponse::LastBlockNumber(bn) => {
                debug!(
                    curr = self.context.head_block_number,
                    new = bn,
                    "received new chain block number"
                );

                if bn > self.context.head_block_number {
                    self.context.head_block_number = bn;
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
    anchor_data: &AnchorData,
) -> BlockEnv {
    let gas_limit = max_gas_limit + ANCHOR_GAS_LIMIT;
    let difficulty = get_difficulty(block_number);

    BlockEnv {
        number: U256::from(block_number),
        coinbase,
        timestamp: U256::from(anchor_data.timestamp),
        gas_limit: U256::from(gas_limit),
        basefee: U256::from(anchor_data.base_fee),
        difficulty: difficulty.into(),
        prevrandao: Some(difficulty),
        blob_excess_gas_and_price: None,
    }
}
