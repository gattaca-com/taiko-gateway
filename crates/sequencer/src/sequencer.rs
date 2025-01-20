//! Manages communication with simulator and sequence incoming transactions

use std::{sync::Arc, thread::sleep, time::Duration};

use alloy_consensus::{Transaction, TxEnvelope};
use alloy_primitives::{utils::format_ether, Address, U256};
use alloy_rpc_types::{Block, Header};
use crossbeam_channel::Receiver;
use eyre::bail;
use pc_common::{
    config::{SequencerConfig, TaikoChainParams, TaikoConfig},
    proposer::NewSealedBlock,
    sequencer::{ExecutionResult, Order},
    taiko::{get_difficulty, get_extra_data, AnchorParams, ParentParams, ANCHOR_GAS_LIMIT},
    types::BlockEnv,
};
use tokio::sync::mpsc::UnboundedSender;
use tracing::{debug, error, info, warn};

use crate::{
    context::{SequencerContext, SAFE_L1_LAG},
    simulator::SimulatorClient,
    txpool::TxPool,
};

const ANCHOR_BATCH_LAG: u64 = 8;

pub struct Sequencer {
    config: SequencerConfig,
    chain_config: TaikoChainParams,
    context: SequencerContext,
    simulator: SimulatorClient,
    /// Receive txs and bundles from RPC
    rpc_rx: Receiver<Order>,
    /// Mempool transactions
    txpool: TxPool,
    /// Send blocks to proposer for inclusion
    new_blocks_tx: UnboundedSender<NewSealedBlock>,
    // Receiver of L1 blocks
    l1_blocks_rx: Receiver<Header>,
    // Receiver of L2 non preconf blocks
    l2_blocks_rx: Receiver<Header>,
}

impl Sequencer {
    pub fn new(
        config: SequencerConfig,
        taiko_config: TaikoConfig,
        rpc_rx: Receiver<Order>,
        mempool_rx: Receiver<Arc<TxEnvelope>>,
        new_blocks_tx: UnboundedSender<NewSealedBlock>,
        l1_blocks_rx: Receiver<Header>,
        l2_blocks_rx: Receiver<Header>,
    ) -> Self {
        let chain_config = taiko_config.params;

        let simulator = SimulatorClient::new(config.simulator_url.clone(), taiko_config);
        // this doesn't handle well the restarts if we had pending orders in the rpc
        let context = SequencerContext::new(config.target_block_time);
        let txpool = TxPool::new(mempool_rx);
        Self {
            config,
            chain_config,
            context,
            simulator,
            rpc_rx,
            txpool,
            new_blocks_tx,
            l1_blocks_rx,
            l2_blocks_rx,
        }
    }

    #[tracing::instrument(skip_all, name = "sequencer")]
    pub fn run(mut self) {
        while !self.is_ready() {
            // ready when we have received a L1 and L2 blocks (parent and last)
            sleep(Duration::from_millis(250));
        }

        loop {
            if let Ok(new_header) = self.l1_blocks_rx.try_recv() {
                self.context.new_l1_block(new_header);
            }

            if let Ok(new_header) = self.l2_blocks_rx.try_recv() {
                self.context.new_l2_block(new_header);
            }

            // fetch txs from mempool
            self.txpool.try_fetch_txs();

            // if missing anchor env, sim it
            if let Err(err) = self.anchor_block() {
                error!(%err, "failed anchoring");
            };

            // check either rpc or a mempool
            self.sequence_next();

            self.commit_seal();
        }
    }

    fn is_ready(&self) -> bool {
        false
    }

    /// Anchors the current block
    fn anchor_block(&mut self) -> eyre::Result<()> {
        if self.context.is_building() {
            // in the middle of a block
            return Ok(());
        }

        let Some((_, anchor_header)) = self.context.l1_headers.first_key_value() else {
            bail!("missing l1 headers");
        };

        let should_refresh_anchor = self.context.current_anchor_id() <
            anchor_header.number + SAFE_L1_LAG - ANCHOR_BATCH_LAG;

        if self.context.is_anchored() && !should_refresh_anchor {
            // anchor is recent enough, keep it as we need to keep the same anchor for the full
            // batch
            return Ok(());
        }

        if should_refresh_anchor {
            let new = AnchorParams {
                block_id: anchor_header.number,
                state_root: anchor_header.state_root,
                timestamp: self.context.parent_l2_header.timestamp.max(anchor_header.timestamp),
            };

            self.context.refresh_anchor(new);
            debug!(anchor =? self.context.anchor, "refreshed anchor");
        }

        let parent_l2_header = &self.context.parent_l2_header;
        let parent = ParentParams {
            timestamp: parent_l2_header.timestamp,
            gas_used: parent_l2_header.gas_used.try_into().unwrap(),
            block_number: parent_l2_header.number,
        };

        debug!(anchor =? self.context.anchor, ?parent, "assembling anchor");

        let (tx, l2_base_fee) = self.simulator.assemble_anchor(parent, self.context.anchor)?;

        let block_number = parent_l2_header.number;
        let block_env = get_block_env_from_anchor(
            block_number,
            self.chain_config.block_max_gas_limit,
            self.config.coinbase_address,
            self.context.anchor.timestamp,
            l2_base_fee,
        )
        .into();

        let extra_data = get_extra_data(self.chain_config.base_fee_config.sharing_pctg);

        match self.simulator.simulate_anchor(tx, block_env, extra_data) {
            Ok(res) => {
                let ExecutionResult::Success { state_id, gas_used, builder_payment } = res else {
                    bail!("failed simulate anchor, res={res:?}");
                };

                debug!(anchor = ?self.context.anchor, params = ?parent, gas_used, builder_payment, "simulated anchor");

                self.context.anchor_current_block(block_number, state_id);
            }
            Err(err) => {
                bail!("failed simulate anchor, err={err}")
            }
        }

        Ok(())
    }

    fn sequence_next(&mut self) {
        if !self.context.is_anchored() {
            return;
        }

        if let Ok(order) = self.rpc_rx.try_recv() {
            self.simulate_tx(order);
        } else if let Some(order) = self.txpool.next_sequence() {
            self.simulate_tx(order);
        }
    }

    fn simulate_tx(&mut self, order: Order) {
        let hash = *order.tx_hash();
        let origin_state_id = self.context.env.tip_state_id;

        match self.simulator.simulate_tx(order, origin_state_id) {
            Ok(res) => match res {
                ExecutionResult::Success { state_id: new_state_id, gas_used, builder_payment } => {
                    debug!(
                        %hash,
                        %origin_state_id,
                        %new_state_id,
                        gas_used,
                        payment = format_ether(builder_payment),
                        "sequenced user tx"
                    );

                    self.context.sequence_tx(new_state_id, gas_used);
                }
                ExecutionResult::Revert { .. } => {
                    debug!(%hash, %origin_state_id, "reverted user tx");
                }
                ExecutionResult::Invalid { reason } => {
                    // TODO: dedup nonces here
                    debug!(%hash, %origin_state_id, reason, "invalid tx");
                }
            },

            Err(err) => {
                error!(%err, "failed simulate tx")
            }
        }
    }

    fn commit_seal(&mut self) {
        if !self.context.is_building() {
            return;
        }

        if !self.context.should_seal() {
            return;
        }

        let origin_state_id = self.context.env.tip_state_id;

        // commit
        match self.simulator.commit_state(origin_state_id) {
            Ok(res) => {
                info!(
                    committed = %origin_state_id,
                    gas_used = res.cumulative_gas_used,
                    payment = format_ether(res.cumulative_builder_payment),
                    "committed"
                );
            }

            Err(err) => {
                error!(%err, "failed commit state");
                return;
            }
        }

        // seal
        let anchor_env = std::mem::take(&mut self.context.env);
        match self.simulator.seal_block() {
            Ok(res) => {
                let block = res.built_block;
                let block_number = block.header.number;

                assert_eq!(anchor_env.block_number, block_number, "block number mismatch");

                info!(
                    bn = block_number,
                    block_hash = %block.header.hash,
                    block_time = ?anchor_env.first_sequenced.elapsed(),
                    payment = format_ether(res.cumulative_builder_payment),
                    "sealed block"
                );

                let txs = block.transactions.txns().map(|tx| (tx.from, tx.nonce()));
                self.txpool.clear_mined(txs);

                self.context.new_l2_block(block.header.clone());

                if !self.config.dry_run {
                    self.send_block_to_proposer(block, self.context.anchor.block_id);
                }
            }

            Err(err) => {
                error!(%err, "failed seal block");
            }
        }
    }

    ///////////////// PROPOSER /////////////////

    fn send_block_to_proposer(&self, block: Arc<Block>, anchor_block_id: u64) {
        if let Err(err) = self.new_blocks_tx.send(NewSealedBlock { block, anchor_block_id }) {
            error!(%err, "failed sequencer->proposer");
        };
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
