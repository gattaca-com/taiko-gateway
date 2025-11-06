use std::time::Duration;

use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types::eth::Transaction;
use alloy_rpc_types_txpool::TxpoolContent;
use crossbeam_channel::Sender;
use eyre::{bail, eyre};
use pc_common::{
    config::{RpcConfig, StaticConfig, Urls},
    metrics::MempoolMetrics,
    runtime::spawn,
    sequencer::Order,
};
use reqwest::Url;
use serde::{Deserialize, Serialize};
use server::RpcServer;
use tokio::time::sleep;
use tracing::{debug, error, info, warn, Instrument};
mod error;
mod server;
use alloy_network::TransactionResponse;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};

pub fn start_rpc(config: &StaticConfig, sequence_tx: Sender<Order>, mempool_tx: Sender<Order>) {
    let rpc_config: RpcConfig = config.into();

    spawn(start_mempool_subscription(
        rpc_config.rpc_url.clone(),
        rpc_config.ws_url.clone(),
        mempool_tx,
    ));
    let rpc_server = RpcServer::new(rpc_config, sequence_tx);

    spawn(rpc_server.run());
}

// TODO: tidy up
#[tracing::instrument(skip_all, name = "mempool")]
async fn start_mempool_subscription(rpc_url: Urls, ws_url: Urls, mempool_tx: Sender<Order>) {
    info!(%rpc_url, %ws_url, "starting mempool subscription fetch");

    let backoff = 4;

    for rpc_url in rpc_url.0 {
        let mempool_tx = mempool_tx.clone();
        spawn(
            async move {
                loop {
                    // refetch full txpool every 30 seconds
                    if let Err(err) = fetch_txpool(rpc_url.clone(), mempool_tx.clone()).await {
                        error!(%err, backoff, "txpool fetch failed. Retrying..");
                    }

                    sleep(Duration::from_secs(30)).await;
                }
            }
            .in_current_span(),
        );
    }

    for ws_url in ws_url.0 {
        let mempool_tx = mempool_tx.clone();
        spawn(
            async move {
                loop {
                    if let Err(err) = subscribe_mempool(ws_url.clone(), mempool_tx.clone()).await {
                        error!(%err, backoff, "mempool sub failed. Retrying..");
                    }

                    MempoolMetrics::ws_reconnect();
                    sleep(Duration::from_secs(backoff)).await;
                }
            }
            .in_current_span(),
        );
    }

    tokio::time::sleep(Duration::MAX).await;
}

async fn subscribe_mempool(ws_url: Url, mempool_tx: Sender<Order>) -> eyre::Result<()> {
    info!(%ws_url, "subscribing to mempool transactions");

    let (ws_stream, _response) = connect_async(ws_url.as_str()).await?;
    let (mut write, mut read) = ws_stream.split();

    // send a sub message
    let subscribe_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["newPendingTransactions", true]
    })
    .to_string();

    write.send(Message::Text(subscribe_message.into())).await?;

    // skip sub confirmation
    read.next().await;

    while let Some(message) = read.next().await {
        let message = message.map_err(|err| eyre!("ws connection error: {:?}", err))?;

        match message {
            Message::Text(raw) => {
                let tx = match serde_json::from_str::<TxMessage>(&raw) {
                    Ok(tx) => tx.params.result,
                    Err(err) => {
                        warn!(%err, %raw, "invalid message from mempool subscription");
                        continue;
                    }
                };

                MempoolMetrics::tx_received();
                // debug!(hash = %tx.inner.tx_hash(), "received from mempool");
                let tx_from = tx.from();
                let _ = mempool_tx.send(Order::new_with_sender(tx.inner.into_inner(), tx_from));
            }

            Message::Ping(bytes) => {
                write.send(Message::Pong(bytes)).await?;
            }

            Message::Close(frame) => {
                if frame.is_some() {
                    bail!("received close frame with data: {frame:?}");
                } else {
                    bail!("ws server closed the connection");
                }
            }

            _ => {
                warn!(?message, "unhandled message from mempool subscription");
            }
        }
    }

    bail!("mempool subscription disconnected");
}

async fn fetch_txpool(rpc_url: Url, mempool_tx: Sender<Order>) -> eyre::Result<()> {
    let provider = ProviderBuilder::new().connect_http(rpc_url);
    let txpool: TxpoolContent = provider.client().request_noparams("txpool_content").await?;

    let mut count = 0;
    for txs in txpool.pending.into_values() {
        for tx in txs.into_values() {
            // debug!(hash = %tx.inner.tx_hash(), "pending txpool");
            let tx_from = tx.from();
            let _ = mempool_tx.send(Order::new_with_sender(tx.inner.into_inner(), tx_from));
            count += 1;
        }
    }

    for txs in txpool.queued.into_values() {
        for tx in txs.into_values() {
            //debug!(hash = %tx.inner.tx_hash(), "queued txpool");
            let tx_from = tx.from();
            let _ = mempool_tx.send(Order::new_with_sender(tx.inner.into_inner(), tx_from));
            count += 1;
        }
    }

    debug!(count, "fetched from txpool");

    Ok(())
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TxMessage {
    pub params: TxNotification,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TxNotification {
    pub result: Transaction,
}

#[cfg(test)]
mod tests {
    use alloy_primitives::address;
    use serde_json::json;

    use super::*;

    #[test]
    fn test_decode() {
        let msg = json!({"jsonrpc":"2.0","method":"eth_subscription","params":{"subscription":"0xc87874441f89ee77d2146fbbc527ea67","result":{"blockHash":null,"blockNumber":null,"from":"0x8cb1d0e5b02e71b6c783ba87fe005a6f2056f5a4","gas":"0x5208","gasPrice":"0x5f5e100","maxFeePerGas":"0x5f5e100","maxPriorityFeePerGas":"0x1","hash":"0x707ba93a2e28a7aad8c4f3f8b950a4125c0665bd1d5e20b05fe0ed76d0dbc712","input":"0x","nonce":"0x5a","to":"0x8cb1d0e5b02e71b6c783ba87fe005a6f2056f5a4","transactionIndex":null,"value":"0x1","type":"0x2","accessList":[],"chainId":"0x28c62","v":"0x0","r":"0x3b5f60e7be4d4a4f70b95f03475f789f6898521c9efe4adf07f8e4c0e358dea5","s":"0x7b7b4cde4a0a5b12fff64672620566295481c42a7836d4091a7fda58cbea7966","yParity":"0x0"}}});
        let tx: Transaction = serde_json::from_value::<TxMessage>(msg).unwrap().params.result;
        assert_eq!(tx.from(), address!("8cb1d0e5b02e71b6c783ba87fe005a6f2056f5a4"));
    }
}
