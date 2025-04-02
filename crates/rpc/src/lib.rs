use std::{str::FromStr, time::Duration};
use alloy_consensus::TxEnvelope;
use alloy_primitives::B256;
use alloy_provider::{Provider, ProviderBuilder};
use alloy_rpc_types_txpool::TxpoolContent;
use crossbeam_channel::{Sender};
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
use tokio::sync::mpsc::{UnboundedSender, UnboundedReceiver};
use std::sync::Arc;
use tokio::sync::Semaphore;

pub fn start_rpc(config: &StaticConfig, sequence_tx: Sender<Order>, mempool_tx: Sender<Order>, hash_tx: UnboundedSender<B256>, hash_rx: UnboundedReceiver<B256>) {
    let rpc_config: RpcConfig = config.into();

    let mempool_tx_clone = mempool_tx.clone();
    spawn(start_mempool_subscription(
        rpc_config.rpc_url.clone(),
        rpc_config.ws_url.clone(),
        mempool_tx,
        hash_tx,
    ));

    spawn(process_tx_hashes(rpc_config.rpc_url.clone(), hash_rx, mempool_tx_clone));
    let rpc_server = RpcServer::new(rpc_config, sequence_tx);

    spawn(rpc_server.run());
}

// TODO: tidy up
#[tracing::instrument(skip_all, name = "mempool")]
async fn start_mempool_subscription(rpc_url: Url, ws_url: Url, mempool_tx: Sender<Order>, hash_tx: UnboundedSender<B256>) {
    info!(%rpc_url, "starting mempool subscription fetch");

    let backoff = 4;

    spawn(
        async move {
            loop {
                // refetch full txpool every 5 minutes
                if let Err(err) = fetch_txpool(rpc_url.clone(), mempool_tx.clone()).await {
                    error!(%err, backoff, "txpool fetch failed. Retrying..");
                }

                sleep(Duration::from_secs(30)).await;
            }
        }
        .in_current_span(),
    );

    loop {
        if let Err(err) = subscribe_mempool(ws_url.clone(), hash_tx.clone()).await {
            error!(%err, backoff, "mempool sub failed. Retrying..");
        }

        MempoolMetrics::ws_reconnect();
        sleep(Duration::from_secs(backoff)).await;
    }
}

async fn process_tx_hashes(rpc_url: Url, mut hash_rx: UnboundedReceiver<B256>, mempool_tx: Sender<Order>) {
    let max_concurrent = 500;
    let semaphore = Arc::new(Semaphore::new(max_concurrent));

    let mut fetched_count = 0;
    while let Some(hash) = hash_rx.recv().await {
         // Clone the semaphore and acquire a permit.
         let semaphore = semaphore.clone();
         let permit = semaphore.acquire_owned().await.unwrap();

        // Clone the values needed for the spawned task.
        let rpc_url_clone = rpc_url.clone();
        let mempool_tx_clone = mempool_tx.clone();
        
        // Spawn a new asynchronous task for each transaction hash.
        tokio::spawn(async move {
            process_tx_hash(rpc_url_clone, hash, mempool_tx_clone).await;
              // When the task completes, dropping the permit will allow another task to start.
              drop(permit);
        });

        fetched_count += 1;
        info!(%fetched_count, "got tx by hash");
    }
}

async fn process_tx_hash(rpc_url: Url, hash: B256, mempool_tx: Sender<Order>) {

    let provider = ProviderBuilder::new().on_http(rpc_url);
    match provider.get_transaction_by_hash(hash).await {
        Ok(tx) => {
            let envelope = TxEnvelope::from(tx.unwrap());
            let _ = mempool_tx.send(Order::new(envelope).unwrap());
        }
        Err(e) => {
            error!(%e, "Failed to fetch transaction using Alloy.");
        }
    }
}
async fn subscribe_mempool(ws_url: Url, hash_tx: UnboundedSender<B256>) -> eyre::Result<()> {
    let mut count = 0;

    info!(ws_url = %ws_url, "subscribing to mempool using tokio-tungstenite");

    let (ws_stream, _response) = connect_async(ws_url.as_str().into_client_request().unwrap()).await?;
    let (mut write, mut read) = ws_stream.split();

    let subscribe_message = serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_subscribe",
        "params": ["newPendingTransactions"]
    })
    .to_string();

    write.send(Message::Text(subscribe_message.into())).await?;

    while let Some(message) = read.next().await {
        count += 1;
        info!(%count, "received message");
        match message {
            Ok(Message::Text(text)) => {
                if let Ok(parsed_message) = serde_json::from_str::<serde_json::Value>(&text) {
                    if let Some(result) = parsed_message.get("params")
                                                        .and_then(|params| params.get("result")) 
                                                        .and_then(|result| result.as_str()) 
                    {
                        let hash = B256::from_str(result).unwrap();
                        let _ = hash_tx.send(hash);
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
