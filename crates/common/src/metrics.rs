use std::{
    net::SocketAddr,
    time::{Duration, Instant},
};

use alloy_consensus::constants::ETH_TO_WEI;
use alloy_primitives::{Address, U256};
use alloy_rpc_types::Header;
use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use eyre::bail;
use lazy_static::lazy_static;
use prometheus::{
    exponential_buckets, linear_buckets, register_gauge_with_registry,
    register_histogram_vec_with_registry, register_histogram_with_registry,
    register_int_counter_vec_with_registry, register_int_counter_with_registry,
    register_int_gauge_vec_with_registry, register_int_gauge_with_registry, Encoder, Gauge,
    Histogram, HistogramVec, IntCounter, IntCounterVec, IntGauge, IntGaugeVec, Opts, Registry,
    TextEncoder,
};
use tokio::net::TcpListener;
use tracing::{error, info};

use crate::{config::L2ChainConfig, runtime::spawn};

pub fn start_metrics_server() {
    let port =
        std::env::var("METRICS_PORT").map(|s| s.parse().expect("invalid port")).unwrap_or(9500);

    spawn(async move {
        if let Err(err) = run_metrics(port).await {
            error!("metrics server error {}", err);
        }
    });
}

pub fn record_info(
    l1_chain_id: u64,
    l2_chain_id: u64,
    operator: Address,
    coinbase: Address,
    l2_config: L2ChainConfig,
) {
    let opts = Opts::new("info", "Gateway info")
        .const_label("version", env!("CARGO_PKG_VERSION"))
        .const_label("commit", env!("GIT_HASH"))
        .const_label("l1_chain_id", l1_chain_id.to_string())
        .const_label("l2_chain_id", l2_chain_id.to_string())
        .const_label("operator", operator.to_string())
        .const_label("coinbase", coinbase.to_string())
        .const_label("taiko_token", l2_config.taiko_token.to_string())
        .const_label("l1_contract", l2_config.l1_contract.to_string())
        .const_label("l2_contract", l2_config.l2_contract.to_string())
        .const_label("router_contract", l2_config.router_contract.to_string())
        .const_label("whitelist_contract", l2_config.whitelist_contract.to_string());

    let info = IntGauge::with_opts(opts).unwrap();
    info.set(1);

    REGISTRY.register(Box::new(info)).unwrap();
}

pub async fn run_metrics(port: u16) -> eyre::Result<()> {
    info!("starting metrics server on port {}", port);

    let router = axum::Router::new()
        .route("/status", get(|| async { StatusCode::OK }))
        .route("/metrics", get(handle_metrics));
    let address = SocketAddr::from(([0, 0, 0, 0], port));
    let listener = TcpListener::bind(&address).await?;

    axum::serve(listener, router).await?;

    bail!("metrics server stopped")
}

async fn handle_metrics() -> Response {
    match prepare_metrics() {
        Ok(response) => response,
        Err(err) => {
            error!(%err, "failed to prepare metrics");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn prepare_metrics() -> Result<Response, MetricsError> {
    let metrics = REGISTRY.gather();
    let encoder = TextEncoder::new();
    let s = encoder.encode_to_string(&metrics)?;

    Response::builder()
        .status(200)
        .header(CONTENT_TYPE, encoder.format_type())
        .body(Body::from(s))
        .map_err(MetricsError::FailedBody)
}

#[derive(Debug, thiserror::Error)]
enum MetricsError {
    #[error("failed encoding metrics {0}")]
    FailedEncoding(#[from] prometheus::Error),

    #[error("failed encoding body {0}")]
    FailedBody(#[from] axum::http::Error),
}

lazy_static! {
    static ref REGISTRY: Registry =
        Registry::new_custom(Some("gateway".to_string()), None).unwrap();

    /////////////////////// SEQUENCER ///////////////////////

    /// State of the gateway
    static ref SEQUENCER_STATE: IntGauge =
        register_int_gauge_with_registry!("sequencer_state", "State of the gateway", &REGISTRY).unwrap();

    /// Is the gateway the current proposer
    static ref IS_PROPOSER: IntGauge =
        register_int_gauge_with_registry!("sequencer_is_proposer", "Gateway is the current proposer", &REGISTRY).unwrap();

    /// Is the gateway the current sequencer
    static ref IS_SEQUENCER: IntGauge =
        register_int_gauge_with_registry!("sequencer_is_sequencer", "Gateway is the current sequencer", &REGISTRY).unwrap();

    /// L1 sync time
    static ref L1_SYNC_TIME: Gauge =
        register_gauge_with_registry!("sequencer_l1_sync_time", "L1 sync time", &REGISTRY).unwrap();

    /// Latency of simulator requests in seconds
    static ref SIMULATOR_LATENCY: HistogramVec =
        register_histogram_vec_with_registry!("sequencer_sim_latency_secs", "Latency in seconds by method", &["method"],
        exponential_buckets(0.0001, 2.0, 16).unwrap(), &REGISTRY).unwrap();

    /// Simulator requests
    static ref SIMULATOR_REQUESTS: IntCounterVec =
        register_int_counter_vec_with_registry!("sequencer_sim_requests", "Request counts to simulator", &[
            "method", "result"
            ], &REGISTRY)
            .unwrap();

    /// Sorting simulations
    static ref SORTING_SIMS: IntGaugeVec =
        register_int_gauge_vec_with_registry!("sequencer_sorting_sims", "Sorting simulation metrics",
        &["type"],
        &REGISTRY
    ).unwrap();

    /////////////////////// RPC / MEMPOOL ///////////////////////

    static ref MEMPOOL_TX_COUNT: IntCounter =
    register_int_counter_with_registry!("mempool_tx_count", "Number mempool tx received", &REGISTRY).unwrap();

    static ref WS_RECONNECTS: IntCounter =
    register_int_counter_with_registry!("mempool_ws_reconnects", "Number of mempool ws reconnects", &REGISTRY).unwrap();

    /////////////////////// BLOCKS ///////////////////////

    static ref L1_BLOCK_NUMBER: IntGauge =
        register_int_gauge_with_registry!("blocks_l1", "L1 block number", &REGISTRY).unwrap();

    static ref L2_BLOCK_NUMBER: IntGauge =
        register_int_gauge_with_registry!("blocks_l2", "L2 block number", &REGISTRY).unwrap();

    static ref L2_ORIGIN_BLOCK_NUMBER: IntGauge =
        register_int_gauge_with_registry!("blocks_l2_origin", "L2 origin block number", &REGISTRY).unwrap();

    static ref BLOCK_COUNT: IntCounter =
        register_int_counter_with_registry!("blocks_built_count", "Block count", &REGISTRY).unwrap();

    static ref BLOCK_BUILD_TIME: Histogram =
        register_histogram_with_registry!("blocks_built_build_time_secs", "Block build time in seconds", exponential_buckets(0.001, 2.0, 16).unwrap(), &REGISTRY).unwrap();

    static ref BLOCK_VALUE: Gauge =
        register_gauge_with_registry!("blocks_built_value", "Block value in ETH", &REGISTRY).unwrap();

    static ref BLOCK_GAS_USED: Gauge =
        register_gauge_with_registry!("blocks_gas_used", "Block gas used", &REGISTRY).unwrap();

    static ref BLOCK_GAS_LIMIT: Gauge =
        register_gauge_with_registry!("blocks_gas_limit", "Block gas limit", &REGISTRY).unwrap();

    static ref BLOCK_BASE_FEE: Gauge =
        register_gauge_with_registry!("blocks_base_fee", "Block base fee", &REGISTRY).unwrap();

    /////////////////////// PROPOSER ///////////////////////

    /// How many batches we have proposed
    static ref PROPOSED_BATCHES: IntCounterVec =
        register_int_counter_vec_with_registry!("proposer_proposed_batches", "Number of batches proposed", &["result"], &REGISTRY).unwrap();

    /// Batch size in bytes
    static ref BATCH_SIZE: Histogram =
        register_histogram_with_registry!("proposer_batch_size_bytes", "Batch size in bytes", exponential_buckets(1.0, 3.0, 16).unwrap(), &REGISTRY).unwrap();

    /// Batch size in blocks
    static ref BATCH_SIZE_BLOCKS: IntCounter =
        register_int_counter_with_registry!("proposer_batch_size_blocks", "Batch size in blocks", &REGISTRY).unwrap();

    /// Proposal latency in seconds
    static ref PROPOSAL_LATENCY: Histogram =
        register_histogram_with_registry!("proposer_proposal_latency_secs", "Proposal latency in seconds", linear_buckets(3.0, 6.0, 12).unwrap(), &REGISTRY).unwrap();

    /// Number of the proposal txs being reorged out
    static ref PROPOSAL_REORGS: IntCounter =
        register_int_counter_with_registry!("proposer_proposal_reorgs", "Number of proposal reorgs", &REGISTRY).unwrap();

    /// Number of resyncs
    static ref RESYNC_COUNTS: IntCounterVec =
        register_int_counter_vec_with_registry!("proposer_resync_counts", "Number of resyncs", &["coinbase"], &REGISTRY).unwrap();

    /// ETH balance
    static ref ETH_BALANCE: Gauge =
        register_gauge_with_registry!("proposer_eth_balance", "ETH balance", &REGISTRY).unwrap();

    /// Taiko token balance
    static ref TOKEN_BALANCE: Gauge =
        register_gauge_with_registry!("proposer_token_balance", "Token balance", &REGISTRY).unwrap();

    /// Token bond in the TaikoInbox contract
    static ref TOKEN_BOND: Gauge =
        register_gauge_with_registry!("proposer_token_bond", "Token bond", &REGISTRY).unwrap();


    /// Prover balance
    static ref PROVER_ETH_BALANCE: Gauge =
        register_gauge_with_registry!("prover_eth_balance", "Prover ETH balance", &REGISTRY).unwrap();

}

pub struct ProposerMetrics;

impl ProposerMetrics {
    pub fn proposed_batches(is_success: bool) {
        PROPOSED_BATCHES.with_label_values(&[if is_success { "success" } else { "failed" }]).inc();
    }

    pub fn batch_size(size: u64) {
        BATCH_SIZE.observe(size as f64);
    }

    pub fn batch_size_blocks(size: u64) {
        BATCH_SIZE_BLOCKS.inc_by(size as u64);
    }

    pub fn proposal_latency(latency: Duration) {
        PROPOSAL_LATENCY.observe(latency.as_secs_f64());
    }

    pub fn proposal_reorg() {
        PROPOSAL_REORGS.inc();
    }

    pub fn resync(is_our_coinbase: bool) {
        RESYNC_COUNTS.with_label_values(&[if is_our_coinbase { "own" } else { "other" }]).inc();
    }

    pub fn eth_balance(balance: U256) {
        ETH_BALANCE.set(f64::from(balance) / ETH_TO_WEI as f64);
    }

    pub fn token_balance(balance: U256) {
        TOKEN_BALANCE.set(f64::from(balance) / ETH_TO_WEI as f64);
    }

    pub fn token_bond(bond: U256) {
        TOKEN_BOND.set(f64::from(bond) / ETH_TO_WEI as f64);
    }

    pub fn prover_eth_balance(balance: U256) {
        PROVER_ETH_BALANCE.set(f64::from(balance) / ETH_TO_WEI as f64);
    }
}

pub struct BlocksMetrics;

impl BlocksMetrics {
    pub fn l1_block_number(l1_block_number: u64) {
        L1_BLOCK_NUMBER.set(l1_block_number as i64);
    }

    pub fn l2_origin_block_number(l2_origin_block_number: u64) {
        L2_ORIGIN_BLOCK_NUMBER.set(l2_origin_block_number as i64);
    }

    pub fn built_block(build_time: Duration, payment: u128) {
        BLOCK_COUNT.inc();
        BLOCK_BUILD_TIME.observe(build_time.as_secs_f64());
        BLOCK_VALUE.set(payment as f64 / ETH_TO_WEI as f64);
    }

    pub fn l2_block(header: &Header) {
        L2_BLOCK_NUMBER.set(header.number as i64);
        BLOCK_GAS_USED.set(header.gas_used as f64);
        BLOCK_GAS_LIMIT.set(header.gas_limit as f64);
        BLOCK_BASE_FEE.set(header.base_fee_per_gas.unwrap_or_default() as f64);
    }
}

pub struct SimulatorMetric<'a> {
    method: &'a str,
    has_completed: bool,
    start: Instant,
}

impl<'a> SimulatorMetric<'a> {
    pub fn new(method: &'a str) -> Self {
        Self { method, has_completed: false, start: Instant::now() }
    }

    pub fn record(&mut self) {
        self.has_completed = true;
    }
}

impl Drop for SimulatorMetric<'_> {
    fn drop(&mut self) {
        if self.has_completed {
            SIMULATOR_REQUESTS.with_label_values(&[self.method, "success"]).inc();

            SIMULATOR_LATENCY
                .with_label_values(&[self.method])
                .observe(self.start.elapsed().as_secs_f64());
        } else {
            SIMULATOR_REQUESTS.with_label_values(&[self.method, "failed"]).inc();
        }
    }
}

pub struct SequencerMetrics;

impl SequencerMetrics {
    pub fn set_state(state: i64) {
        SEQUENCER_STATE.set(state);
    }

    pub fn set_is_proposer(is_proposer: bool) {
        IS_PROPOSER.set(if is_proposer { 1 } else { 0 });
    }

    pub fn set_is_sequencer(is_sequencer: bool) {
        IS_SEQUENCER.set(if is_sequencer { 1 } else { 0 });
    }

    pub fn set_l1_sync_time(l1_sync_time: Duration) {
        L1_SYNC_TIME.set(l1_sync_time.as_secs_f64());
    }

    pub fn record_sim_success() {
        SORTING_SIMS.with_label_values(&["success"]).inc();
    }

    pub fn record_sim_revert() {
        SORTING_SIMS.with_label_values(&["revert"]).inc();
    }

    pub fn record_sim_invalid() {
        SORTING_SIMS.with_label_values(&["invalid"]).inc();
    }

    pub fn record_sim_unknown() {
        SORTING_SIMS.with_label_values(&["unknown"]).inc();
    }
}

pub struct MempoolMetrics;

impl MempoolMetrics {
    pub fn tx_received() {
        MEMPOOL_TX_COUNT.inc();
    }

    pub fn ws_reconnect() {
        WS_RECONNECTS.inc();
    }
}
