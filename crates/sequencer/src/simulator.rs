use std::{future::Future, time::Instant};

use alloy_primitives::Bytes;
use alloy_provider::ProviderBuilder;
use crossbeam_channel::Sender;
use eyre::eyre;
use jsonrpsee::{
    client_transport::ws::Url,
    http_client::{HttpClient, HttpClientBuilder},
};
use pc_common::{
    api::SimulateApiClient,
    config::TaikoConfig,
    metrics::SimulatorMetric,
    runtime::spawn,
    sequencer::{ExecutionResult, Order, SealBlockResponse, StateId},
    taiko::{
        pacaya::{
            self,
            l2::TaikoL2::{self},
        },
        AnchorParams, ParentParams, TaikoL2Client, GOLDEN_TOUCH_ADDRESS,
    },
    types::BlockEnv,
};
use tokio::runtime::Runtime;
use tracing::debug;

use crate::{error::SequencerError, types::SimulatedOrder};

pub struct SimulatorClient {
    runtime: Runtime,
    config: TaikoConfig,
    client: HttpClient,
    taiko_l2: TaikoL2Client,
    sim_tx: Sender<eyre::Result<SimulatedOrder>>,
    sim_url: Url,
}

impl SimulatorClient {
    pub fn new(
        sim_url: Url,
        config: TaikoConfig,
        sim_tx: Sender<eyre::Result<SimulatedOrder>>,
    ) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create runtime");

        let client = HttpClientBuilder::default()
            .build(sim_url.clone())
            .expect("failed building simulator client");

        let provider =
            ProviderBuilder::new().disable_recommended_fillers().on_http(sim_url.clone());
        let taiko_l2 = TaikoL2::new(config.l2_contract, provider);

        Self { runtime, config, client, taiko_l2, sim_tx, sim_url }
    }

    pub fn simulate_anchor(
        &self,
        tx: Order,
        block_env: BlockEnv,
        extra_data: Bytes,
    ) -> eyre::Result<ExecutionResult> {
        debug!(hash = %tx.tx_hash(), ?block_env, ?extra_data, "simulate anchor");

        let mut metric = SimulatorMetric::new("anchor");
        let response = self.block_on(async move {
            self.client
                .simulate_anchor_tx(tx.raw().clone(), block_env, extra_data)
                .await
                .map(|res| res.execution_result)
        })?;
        metric.record();

        Ok(response)
    }

    pub fn spawn_sim_tx(&self, order: Order, state_id: StateId) {
        // debug!(hash = %order.tx_hash(), ?state_id, "simulate tx task");

        let sim_url = self.sim_url.clone();
        let sim_tx = self.sim_tx.clone();
        spawn(async move {
            let mut metric = SimulatorMetric::new("simulate");
            let start = Instant::now();

            // this is spawned on the global runtime and we need to re-initialize the client because
            // it registers the runtime where its first built.
            // TODO: fix this or use another library
            let client = HttpClientBuilder::default()
                .build(sim_url)
                .expect("failed building simulator client");

            let res = client
                .simulate_tx_at_state(order.raw().clone(), state_id)
                .await
                .map(|r| SimulatedOrder {
                    origin_state_id: state_id,
                    execution_result: r.execution_result,
                    order,
                    sim_time: start.elapsed(),
                })
                .map_err(|err| eyre!("failed simulate tx: {err}"));

            if res.is_ok() {
                metric.record();
            }

            let _ = sim_tx.send(res);
        });
    }

    pub fn seal_block(&self, state_id: StateId) -> Result<SealBlockResponse, SequencerError> {
        debug!(%state_id, "seal block");
        let mut metric = SimulatorMetric::new("seal");

        let res = self.block_on(async move { self.client.seal_block(state_id).await })?;

        metric.record();
        Ok(res)
    }

    pub fn assemble_anchor(
        &self,
        timestamp: u64,
        parent: ParentParams,
        anchor: AnchorParams,
    ) -> eyre::Result<(Order, u128)> {
        let l2_base_fee = self.runtime.block_on(pacaya::compute_next_base_fee(
            timestamp,
            &self.taiko_l2,
            self.config.params.base_fee_config,
            parent,
        ))?;

        let tx = pacaya::assemble_anchor_v3(&self.config, parent, anchor, l2_base_fee);
        let order = Order::new_with_sender(tx, GOLDEN_TOUCH_ADDRESS);

        Ok((order, l2_base_fee))
    }

    pub fn block_on<T>(&self, future: impl Future<Output = T>) -> T {
        self.runtime.block_on(future)
    }
}
