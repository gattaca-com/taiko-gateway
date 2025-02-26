//! Builds blocks and sends to proposer loop

use std::sync::Arc;

use crossbeam_channel::Receiver;
use pc_common::{
    config::{SequencerConfig, StaticConfig, TaikoConfig},
    fetcher::BlockFetcher,
    proposer::ProposalRequest,
    runtime::spawn,
    sequencer::Order,
    taiko::lookahead::LookaheadHandle,
};
use sequencer::{Sequencer, SequencerSpine};
use tokio::sync::mpsc::UnboundedSender;
mod context;
mod sequencer;
mod simulator;
mod soft_block;
mod tx_pool;
use alloy_signer_local::PrivateKeySigner;

pub fn start_sequencer(
    config: &StaticConfig,
    taiko_config: TaikoConfig,
    lookahead: LookaheadHandle,
    rpc_rx: Receiver<Arc<Order>>,
    mempool_rx: Receiver<Arc<Order>>,
    new_blocks_tx: UnboundedSender<ProposalRequest>,
    coinbase_signer: PrivateKeySigner,
) {
    let sequencer_config: SequencerConfig = (config, coinbase_signer.address()).into();

    let (l1_blocks_tx, l1_blocks_rx) = crossbeam_channel::unbounded();
    let rpc_url = config.l1.rpc_url.clone();
    let ws_url = config.l1.ws_url.clone();
    spawn(BlockFetcher::new(rpc_url, ws_url, l1_blocks_tx).run("l1", sequencer_config.l1_safe_lag));

    let (l2_blocks_tx, l2_blocks_rx) = crossbeam_channel::unbounded();
    let rpc_url = config.l2.rpc_url.clone();
    let ws_url = config.l2.ws_url.clone();
    spawn(BlockFetcher::new(rpc_url, ws_url, l2_blocks_tx).run("l2", 1));

    let spine = SequencerSpine { rpc_rx, proposer_tx: new_blocks_tx, l1_blocks_rx, l2_blocks_rx };

    let sequencer = Sequencer::new(
        sequencer_config,
        taiko_config,
        mempool_rx,
        spine,
        lookahead,
        coinbase_signer,
    );

    std::thread::Builder::new()
        .name("sequencer".to_string())
        .spawn(move || {
            sequencer.run();
        })
        .expect("failed to start sequencer thread");
}
