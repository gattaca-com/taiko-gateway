use std::{sync::Arc, time::Duration};

use alloy_consensus::TxEnvelope;
use alloy_provider::{Provider, ProviderBuilder, WsConnect};
use alloy_rpc_types_txpool::TxpoolContent;
use crossbeam_channel::Sender;
use pc_common::{
    config::{RpcConfig, StaticConfig},
    runtime::spawn,
    sequencer::Order,
};
use reqwest::Url;
use server::RpcServer;
use tokio::time::sleep;
use tracing::{debug, error, info, Instrument};

mod error;
mod server;

pub fn start_rpc(
    config: &StaticConfig,
    sequence_tx: Sender<Order>,
    mempool_tx: Sender<Arc<TxEnvelope>>,
) {
    let rpc_config: RpcConfig = config.into();

    info!(port = rpc_config.port, "starting RPC server");

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
async fn start_mempool_subscription(
    rpc_url: Url,
    ws_url: Url,
    mempool_tx: Sender<Arc<TxEnvelope>>,
) {
    info!(%rpc_url, "starting mempool subscription fetch");

    let backoff = 4;

    let tx_clone = mempool_tx.clone();
    spawn(
        async move {
            loop {
                if let Err(err) = fetch_txpool(rpc_url.clone(), tx_clone.clone()).await {
                    error!(%err, backoff, "txpool fetch failed. Retrying..");
                }

                sleep(Duration::from_secs(30)).await;
            }
        }
        .in_current_span(),
    );

    loop {
        if let Err(err) = subscribe_mempool(ws_url.clone(), mempool_tx.clone()).await {
            error!(%err, backoff, "mempool sub failed. Retrying..");
        }

        sleep(Duration::from_secs(backoff)).await;
    }
}

async fn subscribe_mempool(rpc_url: Url, mempool_tx: Sender<Arc<TxEnvelope>>) -> eyre::Result<()> {
    info!(rpc_url = %rpc_url, "subscribing to mempool");

    let provider = ProviderBuilder::new().on_ws(WsConnect::new(rpc_url)).await?;

    let mut sub = provider.subscribe_full_pending_transactions().await?;

    while let Ok(tx) = sub.recv().await {
        debug!(hash = %tx.inner.tx_hash(), "pending tx");
        let _ = mempool_tx.send(tx.inner.into());
    }

    Ok(())
}

async fn fetch_txpool(rpc_url: Url, mempool_tx: Sender<Arc<TxEnvelope>>) -> eyre::Result<()> {
    let provider = ProviderBuilder::new().on_http(rpc_url);
    let txpool: TxpoolContent = provider.client().request_noparams("txpool_content").await?;

    let mut count = 0;
    for txs in txpool.pending.into_values() {
        for tx in txs.into_values() {
            debug!(hash = %tx.inner.tx_hash(), "pending txpool");
            let _ = mempool_tx.send(tx.inner.into());
            count += 1;
        }
    }

    for txs in txpool.queued.into_values() {
        for tx in txs.into_values() {
            debug!(hash = %tx.inner.tx_hash(), "queued txpool");
            let _ = mempool_tx.send(tx.inner.into());
            count += 1;
        }
    }

    debug!(count, "fetched from txpool");

    Ok(())
}
