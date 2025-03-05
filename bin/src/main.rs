use std::sync::{atomic::AtomicU64, Arc};

use eyre::eyre;
use pc_common::{
    beacon::init_beacon,
    config::{load_env_vars, load_static_config, EnvConfig, StaticConfig, TaikoConfig},
    erc20::check_and_approve_balance,
    metrics::start_metrics_server,
    runtime::init_runtime,
    taiko::{get_and_validate_config, lookahead::start_looahead_loop},
    utils::{initialize_panic_hook, initialize_tracing_log},
};
use pc_proposer::start_proposer;
use pc_rpc::start_rpc;
use pc_sequencer::start_sequencer;
use tokio::{signal::unix::SignalKind, sync::mpsc};
use tracing::{error, info};

#[tokio::main]
async fn main() {
    initialize_panic_hook();

    let config = load_static_config();
    let envs = load_env_vars();

    let _guard = initialize_tracing_log();
    init_runtime();
    start_metrics_server();

    info!("starting gateway");

    match run(config, envs).await {
        Ok(_) => info!("gateway exited"),
        Err(err) => {
            error!(%err, "gateway exited with error");
            eprintln!("gateway exited with error: {err}");
        }
    }
}

async fn run(config: StaticConfig, envs: EnvConfig) -> eyre::Result<()> {
    info!("{}", serde_json::to_string_pretty(&config)?);

    check_and_approve_balance(
        config.l2.taiko_token,
        config.l2.l1_contract,
        config.l1.rpc_url.clone(),
        envs.proposer_signer_key.clone(),
    )
    .await
    .map_err(|e| eyre!("balance checks: {e}"))?;

    let chain_config = get_and_validate_config(
        config.l1.clone(),
        config.l2.clone(),
        envs.proposer_signer_key.address(),
    )
    .await
    .map_err(|e| eyre!("get config: {e}"))?;

    let beacon_handle = init_beacon(config.l1.beacon_url.clone()).await?;
    let l1_number = Arc::new(AtomicU64::new(0));

    let lookahead = start_looahead_loop(
        config.l1.rpc_url.clone(),
        config.l2.whitelist_contract,
        beacon_handle,
        config.gateway.lookahead,
        l1_number.clone(),
    )
    .await
    .map_err(|e| eyre!("lookahead init: {e}"))?;

    let taiko_config = TaikoConfig::new(&config, chain_config);

    let (new_blocks_tx, new_blocks_rx) = mpsc::unbounded_channel();
    start_proposer(
        &config,
        taiko_config.clone(),
        envs.proposer_signer_key.clone(),
        new_blocks_rx,
        lookahead.clone(),
    )
    .await
    .map_err(|e| eyre!("proposer init: {e}"))?;

    let (rpc_tx, rpc_rx) = crossbeam_channel::unbounded();
    let (mempool_tx, mempool_rx) = crossbeam_channel::unbounded();

    start_sequencer(&config, taiko_config, lookahead, rpc_rx, mempool_rx, new_blocks_tx, l1_number);

    start_rpc(&config, rpc_tx, mempool_tx);

    // Wait for SIGTERM or SIGINT
    let mut sigint = tokio::signal::unix::signal(SignalKind::interrupt())?;
    let mut sigterm = tokio::signal::unix::signal(SignalKind::terminate())?;

    tokio::select! {
        _ = sigint.recv() => {}
        _ = sigterm.recv() => {}
    }

    Ok(())
}
