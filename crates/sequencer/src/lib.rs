//! Builds blocks and sends to proposer loop

use std::sync::{atomic::AtomicU64, Arc};

use crossbeam_channel::Receiver;
use fetcher::BlockFetcher;
use pc_common::{
    config::{SequencerConfig, StaticConfig, TaikoConfig},
    proposer::ProposalRequest,
    runtime::spawn,
    sequencer::Order,
    taiko::lookahead::LookaheadHandle,
};
use sequencer::{Sequencer, SequencerSpine};
use tokio::sync::mpsc::UnboundedSender;
mod context;
mod fetcher;
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
    spawn(BlockFetcher::new(rpc_url, l1_blocks_tx).run_fetch(
        "l1",
        ws_url,
        sequencer_config.l1_safe_lag,
    ));

    let (l2_blocks_tx, l2_blocks_rx) = crossbeam_channel::unbounded();
    let rpc_url = config.l2.rpc_url.clone();
    let ws_url = config.l2.ws_url.clone();
    spawn(BlockFetcher::new(rpc_url, l2_blocks_tx).run_fetch("l2", ws_url, 1));

    let l2_origin = Arc::new(AtomicU64::new(0));
    let (origin_blocks_tx, origin_blocks_rx) = crossbeam_channel::unbounded();
    let rpc_url = config.l2.rpc_url.clone();
    spawn(
        BlockFetcher::new(rpc_url, origin_blocks_tx).run_origin_fetch("origin", l2_origin.clone()),
    );

    let spine = SequencerSpine {
        rpc_rx,
        proposer_tx: new_blocks_tx,
        l1_blocks_rx,
        l2_blocks_rx,
        origin_blocks_rx,
    };

    let sequencer = Sequencer::new(
        sequencer_config,
        taiko_config,
        mempool_rx,
        spine,
        lookahead,
        coinbase_signer,
        l2_origin,
    );

    std::thread::Builder::new()
        .name("sequencer".to_string())
        .spawn(move || {
            sequencer.run();
        })
        .expect("failed to start sequencer thread");
}
