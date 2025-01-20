use pc_common::{
    config::{load_env_vars, load_static_config, EnvConfig, StaticConfig, TaikoConfig},
    metrics::start_metrics_server,
    runtime::init_runtime,
    taiko::get_and_validate_config,
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
            eprintln!("gateway exited with error: {err}");
            error!(%err, "gateway exited with error")
        }
    }
}

async fn run(config: StaticConfig, envs: EnvConfig) -> eyre::Result<()> {
    info!("{}", serde_json::to_string_pretty(&config)?);

    let chain_config = get_and_validate_config(config.l1.clone(), config.l2.clone()).await?;
    info!("initial checks ok");

    let taiko_config = TaikoConfig::new(&config, chain_config);

    let (new_blocks_tx, new_blocks_rx) = mpsc::unbounded_channel();
    start_proposer(&config, taiko_config.clone(), envs.proposer_signer_key, new_blocks_rx).await?;

    let (rpc_tx, rpc_rx) = crossbeam_channel::unbounded();
    let (mempool_tx, mempool_rx) = crossbeam_channel::unbounded();

    start_sequencer(&config, taiko_config, rpc_rx, mempool_rx, new_blocks_tx);
    start_rpc(&config, rpc_tx, mempool_tx);

    // Wait for SIGTERM
    let mut signal = tokio::signal::unix::signal(SignalKind::terminate())?;
    signal.recv().await;

    Ok(())
}
