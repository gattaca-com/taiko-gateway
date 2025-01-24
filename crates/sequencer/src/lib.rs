//! Builds blocks and sends to proposer loop

use crossbeam_channel::Receiver;
use pc_common::{
    config::{SequencerConfig, StaticConfig, TaikoConfig},
    fetcher::BlockFetcher,
    proposer::NewSealedBlock,
    runtime::spawn,
    sequencer::Order,
};
use sequencer::Sequencer;
use tokio::sync::mpsc::UnboundedSender;
mod context;
mod sequencer;
mod simulator;
mod txpool;

pub fn start_sequencer(
    config: &StaticConfig,
    taiko_config: TaikoConfig,
    rpc_rx: Receiver<Order>,
    mempool_rx: Receiver<Order>,
    new_blocks_tx: UnboundedSender<NewSealedBlock>,
) {
    let sequencer_config: SequencerConfig = config.into();

    let (l1_blocks_tx, l1_blocks_rx) = crossbeam_channel::unbounded();
    let rpc_url = config.l1.rpc_url.clone();
    let ws_url = config.l1.ws_url.clone();
    spawn(BlockFetcher::new(rpc_url, ws_url, l1_blocks_tx).run("l1", sequencer_config.l1_safe_lag));

    let (l2_blocks_tx, l2_blocks_rx) = crossbeam_channel::unbounded();
    let rpc_url = config.l2.rpc_url.clone();
    let ws_url = config.l2.ws_url.clone();
    spawn(BlockFetcher::new(rpc_url, ws_url, l2_blocks_tx).run("l2", 1));

    let sequencer = Sequencer::new(
        sequencer_config,
        taiko_config,
        rpc_rx,
        mempool_rx,
        new_blocks_tx,
        l1_blocks_rx,
        l2_blocks_rx,
    );

    std::thread::Builder::new()
        .name("sequencer".to_string())
        .spawn(move || {
            sequencer.run();
        })
        .expect("failed to start sequencer thread");
}
