use std::net::SocketAddr;

use axum::{
    body::Body,
    http::{header::CONTENT_TYPE, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use eyre::bail;
use lazy_static::lazy_static;
use prometheus::{
    register_histogram_vec, register_int_counter_vec, Encoder, HistogramTimer, HistogramVec,
    IntCounterVec, TextEncoder,
};
use tokio::net::TcpListener;
use tracing::{error, info, trace};

use crate::runtime::spawn;

pub fn start_metrics_server() {
    let port =
        std::env::var("METRICS_PORT").map(|s| s.parse().expect("invalid port")).unwrap_or(9500);
    spawn(MetricsProvider::new(port).run());
}

pub struct MetricsProvider {
    port: u16,
}

impl MetricsProvider {
    pub fn new(port: u16) -> Self {
        MetricsProvider { port }
    }

    pub async fn run(self) -> eyre::Result<()> {
        info!("starting metrics server on port {}", self.port);

        let router = axum::Router::new()
            .route("/status", get(|| async { StatusCode::OK }))
            .route("/metrics", get(handle_metrics));
        let address = SocketAddr::from(([0, 0, 0, 0], self.port));
        let listener = TcpListener::bind(&address).await?;

        axum::serve(listener, router).await?;

        bail!("metrics server stopped")
    }
}

async fn handle_metrics() -> Response {
    trace!("handling metrics request");

    match prepare_metrics() {
        Ok(response) => response,
        Err(err) => {
            error!(%err, "failed to prepare metrics");
            StatusCode::INTERNAL_SERVER_ERROR.into_response()
        }
    }
}

fn prepare_metrics() -> Result<Response, MetricsError> {
    let metrics = prometheus::gather();
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
    pub static ref SIMULATOR_LATENCY: HistogramVec =
        register_histogram_vec!("simulator_latency", "Latency by method", &["method"]).unwrap();
    pub static ref SIMULATOR_REQUESTS: IntCounterVec =
        register_int_counter_vec!("simulator_requests", "Request counts to simulator", &[
            "method", "result"
        ])
        .unwrap();
    pub static ref PROPOSER_STATUS_CODES: IntCounterVec =
        register_int_counter_vec!("proposer_status_codes", "Block proposals", &["is_success",])
            .unwrap();
}

pub struct GatewayMetrics;

impl GatewayMetrics {
    pub fn simulate_anchor_timer() -> HistogramTimer {
        SIMULATOR_LATENCY.with_label_values(&["simulate_anchor"]).start_timer()
    }
    pub fn record_simulate_anchor(result: &str) {
        SIMULATOR_REQUESTS.with_label_values(&["simulate_anchor", result]).inc();
    }

    pub fn simulate_tx_timer() -> HistogramTimer {
        SIMULATOR_LATENCY.with_label_values(&["simulate_tx"]).start_timer()
    }
    pub fn record_simulate_tx(result: &str) {
        SIMULATOR_REQUESTS.with_label_values(&["simulate_tx", result]).inc();
    }

    pub fn commit_timer() -> HistogramTimer {
        SIMULATOR_LATENCY.with_label_values(&["commit"]).start_timer()
    }
    pub fn record_commit(result: &str) {
        SIMULATOR_REQUESTS.with_label_values(&["commit", result]).inc();
    }

    pub fn record_proposer_status_code(success: bool) {
        PROPOSER_STATUS_CODES.with_label_values(&[success.to_string().as_str()]).inc();
    }
}
