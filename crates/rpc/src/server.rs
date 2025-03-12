use alloy_primitives::{Bytes, B256};
use crossbeam_channel::Sender;
use jsonrpsee::{core::RpcResult, server::ServerBuilder};
use pc_common::{api::EthApiServer, config::RpcConfig, sequencer::Order};
use tracing::{error, info, Level};

use super::error::RpcError;

pub struct RpcServer {
    /// Port to open the RPC server on
    port: u16,
    /// Channel to send transactions to sequencer
    sequence_tx: Sender<Order>,
}

impl RpcServer {
    pub fn new(config: RpcConfig, sequence_tx: Sender<Order>) -> Self {
        Self { port: config.port, sequence_tx }
    }

    #[tracing::instrument(skip_all, name = "rpc")]
    pub async fn run(self) {
        let addr = format!("0.0.0.0:{}", self.port);
        info!(addr, "starting RPC server");

        let server =
            ServerBuilder::default().build(addr).await.expect("failed to create RPC server");
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
        let order = Order::decode(raw_tx).map_err(|_| RpcError::FailedParsing)?;
        let tx_hash = *order.tx_hash();

        let _ = self.sequence_tx.send(order);

        Ok(tx_hash)
    }
}
