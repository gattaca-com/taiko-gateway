//! Handles creating and landing L1 blockPropose transactions.

use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use client::L1Client;
use manager::ProposerManager;
use pc_common::{
    config::{ProposerConfig, StaticConfig, TaikoConfig},
    proposer::{NewSealedBlock, ProposerContext},
    runtime::spawn,
};
use tokio::sync::mpsc::UnboundedReceiver;

mod client;
mod manager;

pub async fn start_proposer(
    config: &StaticConfig,
    taiko_config: TaikoConfig,
    signer: PrivateKeySigner,
    new_blocks_rx: UnboundedReceiver<NewSealedBlock>,
) -> eyre::Result<()> {
    let proposer_config: ProposerConfig = config.into();

    let context = ProposerContext {
        proposer: signer.address(),
        coinbase: config.gateway.coinbase,
        anchor_input: taiko_config.anchor_input,
    };

    let includer = L1Client::new(
        config.l1.rpc_url.clone(),
        taiko_config.l1_contract,
        signer,
        proposer_config.l1_safe_lag,
        config.l2.router_contract,
    )
    .await?;
    let proposer = ProposerManager::new(proposer_config, context, includer, new_blocks_rx);

    let l2_provider =
        ProviderBuilder::new().disable_recommended_fillers().on_http(taiko_config.rpc_url.clone());
    let preconf_provider = ProviderBuilder::new()
        .disable_recommended_fillers()
        .on_http(taiko_config.preconf_url.clone());

    // start proposer
    proposer.resync(l2_provider, preconf_provider, taiko_config).await?;
    spawn(proposer.run());

    Ok(())
}
