use std::time::Instant;

use alloy_primitives::utils::format_ether;
use jsonrpsee::http_client::{HttpClient, HttpClientBuilder};
use pc_common::{
    api::SimulateApiClient,
    config::WokerConfig,
    metrics::GatewayMetrics,
    runtime::spawn,
    sequencer::{ExecutionResult, Order},
    types::DuplexChannel,
    utils::envelope_to_raw_btyes,
};
use tracing::{debug, error, Instrument};

use crate::{
    context::AnchorEnv,
    types::{WorkerError, WorkerRequest, WorkerResponse},
};

pub struct WorkerManager {
    config: WokerConfig,
    to_sequencer: DuplexChannel<Result<WorkerResponse, WorkerError>, WorkerRequest>,
}

impl WorkerManager {
    pub fn new(
        config: WokerConfig,
        to_sequencer: DuplexChannel<Result<WorkerResponse, WorkerError>, WorkerRequest>,
    ) -> Self {
        Self { config, to_sequencer }
    }

    #[tracing::instrument(skip(self), name = "worker", fields(id = %self.config.id))]
    pub fn run(self) {
        let mut request_id = 0;

        let simulator_client = HttpClientBuilder::default()
            .build(self.config.simulator_url)
            .expect("failed building simulator client");

        while let Ok(request) = self.to_sequencer.rx.recv() {
            let to_sequencer = self.to_sequencer.tx.clone();
            let simulator_client = simulator_client.clone();

            spawn(
                async move {
                    let res = send_request(request, simulator_client.clone(), request_id).await;
                    // ignore error if sequecer is down
                    let _ = to_sequencer.clone().send(res);
                }
                .in_current_span(),
            );

            request_id += 1;
        }

        error!("WorkerManager run loop exited unexpectedly. Is the sequencer down?");
    }
}

#[tracing::instrument(skip_all, name = "req", fields(id = request_id))]
async fn send_request(
    request: WorkerRequest,
    simulator_client: HttpClient,
    request_id: u64,
) -> Result<WorkerResponse, WorkerError> {
    match request {
        WorkerRequest::Simulate { order, state_id: origin_state_id } => {
            let hash = order.hash();
            match order {
                Order::Tx(tx) => {
                    debug!(%origin_state_id, hash = %hash, "sending simulate");

                    let raw_tx = envelope_to_raw_btyes(&tx);

                    let start = Instant::now();
                    let response = simulator_client
                        .simulate_tx_at_state(raw_tx.clone(), origin_state_id)
                        .await;
                    let end = start.elapsed();
                    GatewayMetrics::record_latency("simulate", end);

                    match response {
                        Ok(response) => {
                            debug!(%origin_state_id, time = ?end, ?response, "simulate done");

                            match response.execution_result {
                                ExecutionResult::Success {
                                    state_id: new_state_id,
                                    gas_used,
                                    builder_payment,
                                } => {
                                    GatewayMetrics::record_simulator_request("simulate", "success");

                                    if builder_payment > 0 {
                                        Ok(WorkerResponse::Simulate {
                                            origin_state_id,
                                            new_state_id,
                                            gas_used,
                                            builder_payment,
                                        })
                                    } else {
                                        Err(WorkerError::ZeroPayment(hash))
                                    }
                                }

                                ExecutionResult::Revert {
                                    state_id: _,
                                    gas_used,
                                    builder_payment: _,
                                } => {
                                    GatewayMetrics::record_simulator_request("simulate", "revert");
                                    Err(WorkerError::Revert(gas_used))
                                }

                                ExecutionResult::Invalid { reason } => {
                                    GatewayMetrics::record_simulator_request("simulate", "invalid");
                                    Err(WorkerError::InvalidOrHalt(reason))
                                }
                            }
                        }

                        Err(err) => {
                            error!(%origin_state_id, %err, %hash, "failed simulate");
                            GatewayMetrics::record_simulator_request("simulate", "error");
                            Err(WorkerError::Connection)
                        }
                    }
                }
            }
        }

        WorkerRequest::Anchor { tx, anchor_data, block_env, extra_data } => {
            let hash = tx.tx_hash();
            let env = serde_json::to_string(&block_env).unwrap();

            let block_number: u64 = block_env.number.try_into().unwrap();

            debug!(anchor_ts=anchor_data.timestamp, bn=block_number, %env, hash = %hash, "sending simulate anchor");

            let raw_tx = envelope_to_raw_btyes(&tx);

            let start = Instant::now();
            let response =
                simulator_client.simulate_anchor_tx(raw_tx, *block_env, extra_data).await;
            let end = start.elapsed();
            GatewayMetrics::record_latency("anchor", end);

            match response {
                Ok(response) => match response.execution_result {
                    ExecutionResult::Success { state_id, gas_used: _, builder_payment: _ } => {
                        debug!(block_number, time = ?end, ?response, "simulate anchor done");
                        let env = AnchorEnv::new(block_number, anchor_data, state_id);
                        GatewayMetrics::record_simulator_request("anchor", "success");

                        Ok(WorkerResponse::Anchor { env })
                    }

                    ExecutionResult::Revert { .. } => {
                        GatewayMetrics::record_simulator_request("anchor", "revert");
                        Err(WorkerError::FailedAnchorSim(response))
                    }

                    ExecutionResult::Invalid { .. } => {
                        GatewayMetrics::record_simulator_request("anchor", "invalid");
                        Err(WorkerError::FailedAnchorSim(response))
                    }
                },

                Err(err) => {
                    error!(block_number, %err, %hash, "failed simulate anchor");
                    GatewayMetrics::record_simulator_request("anchor", "error");
                    Err(WorkerError::Connection)
                }
            }
        }

        WorkerRequest::Commit { state_id } => {
            debug!(%state_id, "sending commit");

            let start = Instant::now();
            let response = simulator_client.commit_state(state_id).await;
            let end = start.elapsed();
            GatewayMetrics::record_latency("commit", end);

            match response {
                Ok(response) => {
                    debug!(%state_id, time = ?end, ?response, "commit done");
                    GatewayMetrics::record_simulator_request("commit", "success");
                    Ok(WorkerResponse::Commit { commit: response, origin_state_id: state_id })
                }

                Err(err) => {
                    error!(%err, "failed commit");
                    GatewayMetrics::record_simulator_request("commit", "error");
                    Err(WorkerError::Connection)
                }
            }
        }

        WorkerRequest::Seal => {
            debug!("sending seal");

            let start = Instant::now();
            let response = simulator_client.seal_block().await;
            let end = start.elapsed();
            GatewayMetrics::record_latency("seal", end);

            match response {
                Ok(response) => {
                    debug!(time = ?end, builder_payment=format_ether(response.cumulative_builder_payment), "seal done");
                    GatewayMetrics::record_simulator_request("seal", "success");
                    Ok(WorkerResponse::Block(response))
                }

                Err(err) => {
                    error!(%err, "failed seal");
                    GatewayMetrics::record_simulator_request("seal", "error");
                    Err(WorkerError::Connection)
                }
            }
        }
    }
}
