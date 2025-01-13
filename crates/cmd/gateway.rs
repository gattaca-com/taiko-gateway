use pc_common::{
    config::{load_env_vars, load_static_config, EnvConfig, StaticConfig},
    metrics::start_metrics_server,
    runtime::init_runtime,
    taiko::get_and_validate_config,
    types::DuplexChannel,
    utils::{initialize_panic_hook, initialize_tracing_log},
};
use pc_driver::start_driver;
use pc_proposer::start_proposer;
use pc_rpc::start_rpc;
use pc_sequencer::start_sequencer;
use tokio::signal::unix::SignalKind;
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

    let chain_config = get_and_validate_config(config.l1.clone(), (&config).into()).await?;
    info!("initial checks ok");

    let (to_proposer, to_sequencer) = DuplexChannel::new();
    start_proposer(&config, chain_config.clone(), envs.proposer_signer_key, to_sequencer).await?;

    let (rpc_tx, rpc_rx) = crossbeam_channel::unbounded();
    let (mempool_tx, mempool_rx) = crossbeam_channel::unbounded();
    let (to_driver, to_sequencer) = DuplexChannel::new();
    start_sequencer(&config, chain_config.clone(), rpc_rx, mempool_rx, to_proposer, to_driver);
    start_driver(&config, chain_config, to_sequencer);
    start_rpc(&config, rpc_tx, mempool_tx);

    // Wait for SIGTERM
    let mut signal = tokio::signal::unix::signal(SignalKind::terminate())?;
    signal.recv().await;

    Ok(())
}
