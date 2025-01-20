//! Handles creating and landing L1 blockPropose transactions.

use alloy_signer_local::PrivateKeySigner;
use client::L1Client;
use manager::ProposerManager;
use pc_common::{
    config::{StaticConfig, TaikoConfig},
    proposer::{NewSealedBlock, ProposerContext},
    runtime::spawn,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::info;

mod client;
mod manager;

pub async fn start_proposer(
    config: &StaticConfig,
    taiko_config: TaikoConfig,
    signer: PrivateKeySigner,
    new_blocks_rx: UnboundedReceiver<NewSealedBlock>,
) -> eyre::Result<()> {
    let proposer_config = config.into();

    info!(proposer_address = %signer.address(), "starting l1 proposer");

    let context = ProposerContext {
        proposer: signer.address(),
        coinbase: config.gateway.coinbase,
        anchor_input: taiko_config.anchor_input,
    };

    let includer =
        L1Client::new(config.l1.rpc_url.clone(), taiko_config.l1_contract, signer).await?;

    // Stale block sync
    // resync_blocks(
    //     config.l1.rpc_url.clone(),
    //     taiko_config,
    //     taiko_proposer.clone(),
    //     includer.clone(),
    // )
    // .await?;

    // Start proposer
    let proposer = ProposerManager::new(proposer_config, context, includer, new_blocks_rx);
    spawn(proposer.run());

    Ok(())
}
