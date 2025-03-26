//! Builds blocks and sends to proposer loop

use std::sync::{atomic::AtomicU64, Arc};

use alloy_primitives::Address;
use crossbeam_channel::Receiver;
use fetcher::BlockFetcher;
use jwt::parse_secret_from_file;
use pc_common::{
    config::{SequencerConfig, StaticConfig, TaikoConfig},
    proposer::ProposalRequest,
    runtime::spawn,
    sequencer::Order,
    taiko::lookahead::LookaheadHandle,
};
use sequencer::Sequencer;
use tokio::sync::mpsc::UnboundedSender;
mod context;
mod fetcher;
mod jwt;
mod sequencer;
mod simulator;
mod soft_block;
mod sorting;
mod tx_pool;
mod types;

use tracing::error;
use types::SequencerSpine;

#[allow(clippy::too_many_arguments)]
pub fn start_sequencer(
    config: &StaticConfig,
    taiko_config: TaikoConfig,
    lookahead: LookaheadHandle,
    rpc_rx: Receiver<Order>,
    mempool_rx: Receiver<Order>,
    new_blocks_tx: UnboundedSender<ProposalRequest>,
    l1_number: Arc<AtomicU64>,
    operator_address: Address,
) {
    let mut jwt_secret = Vec::new();

    // If jwt_path is not empty, read and add Authorization header
    if !config.gateway.jwt_secret_path.as_os_str().is_empty() {
        match parse_secret_from_file(config.gateway.jwt_secret_path.clone()) {
            Ok(secret) => {
                jwt_secret = secret;
            }
            Err(e) => error!("Error loading secret: {}", e),
        }
    }

    let sequencer_config: SequencerConfig = (config, jwt_secret, operator_address).into();

    let (l1_headers_tx, l1_blocks_rx) = crossbeam_channel::unbounded();
    let rpc_url = config.l1.rpc_url.clone();
    let ws_url = config.l1.ws_url.clone();
    spawn(BlockFetcher::new(rpc_url).run_fetch(
        "l1",
        ws_url,
        sequencer_config.l1_safe_lag,
        l1_headers_tx,
    ));

    let (l2_blocks_tx, l2_blocks_rx) = crossbeam_channel::unbounded();
    let rpc_url = config.l2.rpc_url.clone();
    let ws_url = config.l2.ws_url.clone();
    spawn(BlockFetcher::new(rpc_url).run_full_fetch("l2", ws_url, 1, l2_blocks_tx));

    let l2_origin = Arc::new(AtomicU64::new(0));
    let (origin_blocks_tx, origin_blocks_rx) = crossbeam_channel::unbounded();
    let rpc_url = config.l2.rpc_url.clone();
    spawn(BlockFetcher::new(rpc_url).run_origin_fetch(
        "origin",
        origin_blocks_tx,
        l2_origin.clone(),
    ));

    let (sim_tx, sim_rx) = crossbeam_channel::unbounded();
    let spine = SequencerSpine {
        rpc_rx,
        mempool_rx,
        proposer_tx: new_blocks_tx,
        l1_blocks_rx,
        l2_blocks_rx,
        origin_blocks_rx,
        sim_rx,
    };

    let sequencer = Sequencer::new(
        sequencer_config,
        taiko_config,
        spine,
        lookahead,
        l2_origin,
        l1_number,
        sim_tx,
    );

    std::thread::Builder::new()
        .name("sequencer".to_string())
        .spawn(move || {
            sequencer.run();
        })
        .expect("failed to start sequencer thread");
}
