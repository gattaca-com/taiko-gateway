use std::{str::FromStr, time::Duration};
use alloy_consensus::TxEnvelope;
use alloy_primitives::B256;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_txpool::TxpoolContent;
use crossbeam_channel::Sender;
use pc_common::{
    config::{RpcConfig, StaticConfig},
    metrics::MempoolMetrics,
    runtime::spawn,
    sequencer::Order,
};
use reqwest::Url;
use server::RpcServer;
use tokio::time::sleep;
use tracing::{debug, error, info, Instrument};
mod error;
mod server;
use tokio_tungstenite::{connect_async, tungstenite::{client::IntoClientRequest, protocol::Message}};
use futures_util::{StreamExt, SinkExt};

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
async fn start_mempool_subscription(rpc_url: Url, ws_url: Url, mempool_tx: Sender<Order>) {
    info!(%rpc_url, "starting mempool subscription fetch");

    let backoff = 4;

    let tx_clone = mempool_tx.clone();
    let rpc_url_clone = rpc_url.clone();
    spawn(
        async move {
            loop {
                // refetch full txpool every 5 minutes
                if let Err(err) = fetch_txpool(rpc_url.clone(), tx_clone.clone()).await {
                    error!(%err, backoff, "txpool fetch failed. Retrying..");
                }

                sleep(Duration::from_secs(30)).await;
            }
        }
        .in_current_span(),
    );

    loop {
        if let Err(err) = subscribe_mempool(rpc_url_clone.clone(), ws_url.clone(), mempool_tx.clone()).await {
            error!(%err, backoff, "mempool sub failed. Retrying..");
        }

        MempoolMetrics::ws_reconnect();
        sleep(Duration::from_secs(backoff)).await;
    }
}

async fn subscribe_mempool(rpc_url: Url, ws_url: Url, mempool_tx: Sender<Order>) -> eyre::Result<()> {
    info!(ws_url = %ws_url, "subscribing to mempool using tokio-tungstenite");

    let (ws_stream, _response) = connect_async(ws_url.as_str().into_client_request().unwrap()).await?;
    let (mut write, mut read) = ws_stream.split();

    // Send a subscription message to listen to pending transactions.
    let subscribe_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["newPendingTransactions"]
    })
    .to_string();

    let provider = ProviderBuilder::new().on_http(rpc_url);
    
    write.send(Message::Text(subscribe_message.into())).await?;

    while let Some(message) = read.next().await {
        match message {
            Ok(Message::Text(text)) => {
                if let Ok(parsed_message) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(result) = parsed_message.get("params")
                                                        .and_then(|params| params.get("result")) 
                                                        .and_then(|result| result.as_str()) 
                    {
                        let hash = B256::from_str(result).unwrap();

                        // Fetch the transaction by hash using Alloy's client
                        match provider.get_transaction_by_hash(hash).await {
                            Ok(tx) => {
                                let envelope = TxEnvelope::from(tx.unwrap());
                                let _ = mempool_tx.send(Order::new(envelope).unwrap());


                                MempoolMetrics::tx_received();
                            }
                            Err(e) => {
                                error!(%e, "Failed to fetch transaction using Alloy.");
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!(%e, "WebSocket connection error.");
                break;
            }
            _ => {}
        }
    }

    Ok(())
}
async fn fetch_txpool(rpc_url: Url, mempool_tx: Sender<Order>) -> eyre::Result<()> {
    let provider = ProviderBuilder::new().on_http(rpc_url);
    let txpool: TxpoolContent = provider.client().request_noparams("txpool_content").await?;

    let mut count = 0;
    for txs in txpool.pending.into_values() {
        for tx in txs.into_values() {
            // debug!(hash = %tx.inner.tx_hash(), "pending txpool");
            let _ = mempool_tx.send(Order::new_with_sender(tx.inner, tx.from));
            count += 1;
        }
    }

    for txs in txpool.queued.into_values() {
        for tx in txs.into_values() {
            //debug!(hash = %tx.inner.tx_hash(), "queued txpool");
            let _ = mempool_tx.send(Order::new_with_sender(tx.inner, tx.from));
            count += 1;
        }
    }

    debug!(count, "fetched from txpool");

    Ok(())
}
