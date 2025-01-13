//! Handles creating and landing L1 blockPropose transactions.

use std::sync::Arc;

use alloy_signer_local::PrivateKeySigner;
use includer::L1Includer;
use manager::{propose_pending_blocks, resync_blocks, Proposer};
use pc_common::{
    config::{StaticConfig, TaikoChainConfig, TaikoConfig},
    proposer::ProposerRequest,
    runtime::spawn,
    sequencer::SequenceLoop,
    types::DuplexChannel,
};
use tracing::info;
use types::{Propose, ProposeData};

mod includer;
mod manager;
mod taiko;
mod types;

pub async fn start_proposer(
    config: &StaticConfig,
    chain_config: TaikoChainConfig,
    signer: PrivateKeySigner,
    to_sequencer: DuplexChannel<SequenceLoop, ProposerRequest>,
) -> eyre::Result<()> {
    let proposer_config = config.into();
    let taiko_config: TaikoConfig = config.into();
    let use_blobs = config.gateway.use_blobs;
    let force_reorgs = config.gateway.force_reorgs;
    let propose_data = ProposeData::new(config.gateway.coinbase);

    info!(proposer_address = %signer.address(), "starting l1 proposer");

    let includer = Arc::new(L1Includer::new(config.l1.rpc_url.clone(), signer));
    let propose = Propose::new_contract(taiko_config, propose_data);
    let taiko_proposer = Arc::new(propose);

    // Stale block sync
    resync_blocks(
        config.l1.rpc_url.clone(),
        config.gateway.simulator_url.clone(),
        config.l2.rpc_url.clone(),
        chain_config,
        taiko_proposer.clone(),
        includer.clone(),
        force_reorgs,
    )
    .await?;

    // Start proposer
    let (block_tx, block_rx) = tokio::sync::mpsc::unbounded_channel();
    spawn(propose_pending_blocks(
        block_rx,
        taiko_proposer,
        includer,
        use_blobs,
        config.gateway.use_batch,
    ));
    let proposer = Proposer::new(proposer_config, block_tx, to_sequencer);
    std::thread::Builder::new()
        .name("proposer".to_string())
        .spawn(move || {
            proposer.run();
        })
        .expect("failed to start proposer thread");

    Ok(())
}
