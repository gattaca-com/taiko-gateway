#![allow(clippy::blocks_in_conditions)]

use alloy_consensus::TxEnvelope;
use alloy_eips::{eip2718::Decodable2718, BlockId, BlockNumberOrTag};
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_rpc_types::{Block, TransactionReceipt};
use crossbeam_channel::Sender;
use jsonrpsee::{
    core::RpcResult,
    http_client::{HttpClient, HttpClientBuilder},
    server::ServerBuilder,
};
use pc_common::{
    api::{EthApiClient, EthApiServer},
    config::RpcConfig,
    sequencer::Order,
};
use tracing::{error, trace, Level};

use super::error::RpcError;

pub struct RpcServer {
    /// Port to open the RPC server on
    port: u16,

    /// Channel to send transactions to sequencer
    sequence_tx: Sender<Order>,
    /// Client to local simulator to serve state requests
    /// NOTE: this is a temporary passthrough, in practice we should at least
    /// keep a local cache to avoid spamming the simulator
    /// Also we're wasting a deserialization, could probably fwd it directly
    simulator_client: HttpClient,
}

impl RpcServer {
    pub fn new(config: RpcConfig, sequence_tx: Sender<Order>) -> Self {
        let simulator_client = HttpClientBuilder::default()
            .build(config.simulator_url)
            .expect("failed building simulator client");

        Self { port: config.port, sequence_tx, simulator_client }
    }

    #[tracing::instrument(skip_all, name = "rpc")]
    pub async fn run(self) {
        let address = format!("0.0.0.0:{}", self.port);

        let server =
            ServerBuilder::default().build(address).await.expect("failed to create RPC server");
        let execution_module = EthApiServer::into_rpc(self);

        let server_handle = server.start(execution_module);
        server_handle.stopped().await;

        error!("RPC server stopped");
    }
}

#[async_trait::async_trait]
impl EthApiServer for RpcServer {
    #[tracing::instrument(skip_all, name = "raw_transaction", err, ret(level = Level::DEBUG))]
    async fn send_raw_transaction(&self, raw_tx: Bytes) -> RpcResult<B256> {
        let tx = TxEnvelope::network_decode(&mut raw_tx.as_ref())
            .map_err(|_| RpcError::FailedParsing)?;
        let tx_hash = *tx.tx_hash();

        if let Err(err) = self.sequence_tx.send(Order::Tx(tx.into())) {
            error!(%err, "failed to send tx to sequencer" );
        }

        Ok(tx_hash)
    }

    #[tracing::instrument(skip(self), name = "tx_receipt", err, ret(level = Level::TRACE))]
    async fn transaction_receipt(&self, hash: B256) -> RpcResult<Option<TransactionReceipt>> {
        self.simulator_client.transaction_receipt(hash).await.map_err(|err| {
            error!(%err, "failed to get result from simulator");
            RpcError::ClientError(err).into()
        })
    }

    #[tracing::instrument(skip(self), name = "block_number", err, ret(level = Level::DEBUG))]
    async fn block_number(&self) -> RpcResult<U256> {
        self.simulator_client.block_number().await.map_err(|err| {
            error!(%err, "failed to get result from simulator");
            RpcError::ClientError(err).into()
        })
    }

    #[tracing::instrument(skip(self), name = "block_by_number", err)]
    async fn block_by_number(
        &self,
        number: BlockNumberOrTag,
        full: bool,
    ) -> RpcResult<Option<Block>> {
        trace!(?number, full);
        self.simulator_client.block_by_number(number, full).await.map_err(|err| {
            error!(%err, "failed to get result from simulator");
            RpcError::ClientError(err).into()
        })
    }

    #[tracing::instrument(skip(self), name = "block_by_hash", err, ret(level = Level::TRACE))]
    async fn block_by_hash(&self, hash: B256, full: bool) -> RpcResult<Option<Block>> {
        self.simulator_client.block_by_hash(hash, full).await.map_err(|err| {
            error!(%err, "failed to get result from simulator");
            RpcError::ClientError(err).into()
        })
    }

    #[tracing::instrument(skip(self), name = "tx_count", err, ret(level = Level::TRACE))]
    async fn transaction_count(
        &self,
        address: Address,
        block_number: Option<BlockId>,
    ) -> RpcResult<U256> {
        self.simulator_client.transaction_count(address, block_number).await.map_err(|err| {
            error!(%err, "failed to get result from simulator");
            RpcError::ClientError(err).into()
        })
    }

    #[tracing::instrument(skip(self), name = "balance", err, ret(level = Level::TRACE))]
    async fn balance(&self, address: Address, block_number: Option<BlockId>) -> RpcResult<U256> {
        self.simulator_client.balance(address, block_number).await.map_err(|err| {
            error!(%err, "failed to get result from simulator");
            RpcError::ClientError(err).into()
        })
    }
}
