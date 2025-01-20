use alloy_primitives::Bytes;
use alloy_provider::ProviderBuilder;
use eyre::eyre;
use jsonrpsee::{
    client_transport::ws::Url,
    http_client::{HttpClient, HttpClientBuilder},
};
use pc_common::{
    api::SimulateApiClient,
    config::TaikoConfig,
    sequencer::{CommitStateResponse, ExecutionResult, Order, SealBlockResponse, StateId},
    taiko::{
        pacaya::{
            self,
            l2::TaikoL2::{self},
        },
        AnchorParams, ParentParams, TaikoL2Client,
    },
    types::BlockEnv,
};
use tokio::runtime::Runtime;
use tracing::debug;

pub struct SimulatorClient {
    config: TaikoConfig,
    runtime: Runtime,
    client: HttpClient,
    taiko_l2: TaikoL2Client,
}

impl SimulatorClient {
    pub fn new(simulator_url: Url, config: TaikoConfig) -> Self {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("failed to create runtime");

        let client = HttpClientBuilder::default()
            .build(simulator_url.clone())
            .expect("failed building simulator client");

        let provider = ProviderBuilder::new().on_http(simulator_url);
        let taiko_l2 = TaikoL2::new(config.l2_contract, provider);

        Self { config, runtime, client, taiko_l2 }
    }

    pub fn simulate_anchor(
        &self,
        tx: Order,
        block_env: BlockEnv,
        extra_data: Bytes,
    ) -> eyre::Result<ExecutionResult> {
        debug!(hash = %tx.tx_hash(), "simulate anchor");

        let response = self.runtime.block_on(async move {
            self.client
                .simulate_anchor_tx(tx.raw().clone(), block_env, extra_data)
                .await
                .map(|res| res.execution_result)
        })?;

        Ok(response)
    }

    /// Simulate a tx at a given state id
    pub fn simulate_tx(&self, order: Order, state_id: StateId) -> eyre::Result<ExecutionResult> {
        let response = self.runtime.block_on(async move {
            self.client
                .simulate_tx_at_state(order.raw().clone(), state_id)
                .await
                .map(|res| res.execution_result)
        })?;

        Ok(response)
    }

    pub fn commit_state(&self, state_id: StateId) -> eyre::Result<CommitStateResponse> {
        debug!(%state_id, "commit state");

        self.runtime.block_on(async move {
            self.client.commit_state(state_id).await.map_err(|err| eyre!("commit state err: {err}"))
        })
    }

    pub fn seal_block(&self) -> eyre::Result<SealBlockResponse> {
        debug!("seal block");

        self.runtime.block_on(async move {
            self.client.seal_block().await.map_err(|err| eyre!("seal block err: {err}"))
        })
    }

    pub fn assemble_anchor(
        &self,
        parent: ParentParams,
        anchor: AnchorParams,
    ) -> eyre::Result<(Order, u128)> {
        let l2_base_fee = self.runtime.block_on(pacaya::compute_next_base_fee(
            &self.taiko_l2,
            self.config.params.base_fee_config,
            parent,
            anchor,
        ))?;

        let tx = pacaya::assemble_anchor_v3(
            &self.config,
            parent,
            anchor,
            l2_base_fee,
            self.config.anchor_input,
        );

        let tx = Order::new(tx.into());
        Ok((tx, l2_base_fee))
    }
}
