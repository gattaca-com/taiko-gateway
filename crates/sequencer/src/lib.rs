//! Manages communication with simulator and sequence incoming transactions

use std::sync::Arc;

use alloy_consensus::TxEnvelope;
use crossbeam_channel::Receiver;
use pc_common::{
    config::{StaticConfig, TaikoChainConfig},
    driver::{DriverRequest, DriverResponse},
    proposer::ProposerRequest,
    sequencer::{Order, SequenceLoop},
    types::DuplexChannel,
};
use sequencer::Sequencer;
use tracing::info;
use worker::WorkerManager;

mod context;
mod sequencer;
mod txpool;
mod types;
mod worker;

pub fn start_sequencer(
    config: &StaticConfig,
    chain_config: TaikoChainConfig,
    rpc_rx: Receiver<Order>,
    mempool_rx: Receiver<Arc<TxEnvelope>>,
    to_proposer: DuplexChannel<ProposerRequest, SequenceLoop>,
    to_driver: DuplexChannel<DriverRequest, DriverResponse>,
) {
    let sequencer_config = config.into();
    let worker_config = config.into();

    info!("starting sequencer");

    let (to_worker, to_sequencer) = DuplexChannel::new();
    let worker = WorkerManager::new(worker_config, to_sequencer);
    std::thread::Builder::new()
        .name("worker".to_string())
        .spawn(move || {
            worker.run();
        })
        .expect("failed to start worker thread");

    let sequencer = Sequencer::new(
        sequencer_config,
        chain_config,
        rpc_rx,
        mempool_rx,
        to_proposer,
        to_worker,
        to_driver,
    );

    std::thread::Builder::new()
        .name("sequencer".to_string())
        .spawn(move || {
            sequencer.run();
        })
        .expect("failed to start sequencer thread");
}
