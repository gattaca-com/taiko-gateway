use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use alloy_eips::BlockNumberOrTag;
use alloy_primitives::U256;
use alloy_provider::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy_rpc_types::{Block, BlockTransactionsKind, Header};
use crossbeam_channel::Sender;
use eyre::OptionExt;
use pc_common::metrics::BlocksMetrics;
use reqwest::Url;
use serde::Deserialize;
use tokio::time::sleep;
use tracing::{debug, error, info};

/// Fetches new blocks from rpc
// TODO: fix error flows
pub struct BlockFetcher {
    url: Url,
}

impl BlockFetcher {
    pub fn new(rpc_url: Url) -> Self {
        Self { url: rpc_url }
    }

    #[tracing::instrument(skip_all, name = "block_fetch" fields(id = %id))]
    pub async fn run_fetch(
        self,
        id: &str,
        ws_url: Url,
        fetch_last: u64,
        new_block_tx: Sender<Header>,
    ) {
        info!(fetch_last, rpc_url = %self.url, %ws_url, "starting block fetch");

        let backoff = 4;

        let provider =
            ProviderBuilder::new().disable_recommended_fillers().on_http(self.url.clone());

        let last_block = provider.get_block_number().await.expect("failed to fetch last block");

        while let Err(err) =
            fetch_last_headers(&provider, new_block_tx.clone(), last_block - fetch_last, last_block)
                .await
        {
            error!(%err, backoff, "block fetch loop failed. Retrying..");
            sleep(Duration::from_secs(backoff)).await;
        }

        loop {
            if let Err(err) = subscribe_headers(ws_url.clone(), new_block_tx.clone()).await {
                error!(%err, backoff, "block fetch loop failed. Retrying..");
            }

            sleep(Duration::from_secs(backoff)).await;
        }
    }

    #[tracing::instrument(skip_all, name = "block_fetch" fields(id = %id))]
    pub async fn run_full_fetch(
        self,
        id: &str,
        ws_url: Url,
        fetch_last: u64,
        new_block_tx: Sender<Block>,
    ) {
        info!(fetch_last, rpc_url = %self.url, %ws_url, "starting block fetch");

        let backoff = 4;

        let provider =
            ProviderBuilder::new().disable_recommended_fillers().on_http(self.url.clone());

        let last_block = provider.get_block_number().await.expect("failed to fetch last block");

        while let Err(err) =
            fetch_last_blocks(&provider, new_block_tx.clone(), last_block - fetch_last, last_block)
                .await
        {
            error!(%err, backoff, "block fetch loop failed. Retrying..");
            sleep(Duration::from_secs(backoff)).await;
        }

        loop {
            if let Err(err) =
                subscribe_blocks(self.url.clone(), ws_url.clone(), new_block_tx.clone()).await
            {
                error!(%err, backoff, "block fetch loop failed. Retrying..");
            }

            sleep(Duration::from_secs(backoff)).await;
        }
    }

    #[tracing::instrument(skip_all, name = "origin_fetch" fields(id = %id))]
    pub async fn run_origin_fetch(
        self,
        id: &str,
        new_header_tx: Sender<Header>,
        l2_origin: Arc<AtomicU64>,
    ) {
        info!(rpc_url = %self.url, "starting preconf check fetch");

        let backoff = 4;

        loop {
            if let Err(err) =
                fetch_origin_blocks(self.url.clone(), new_header_tx.clone(), l2_origin.clone())
                    .await
            {
                error!(%err, backoff, "block fetch loop failed. Retrying..");
            }

            sleep(Duration::from_secs(backoff)).await;
        }
    }
}

async fn fetch_last_headers(
    provider: &RootProvider,
    tx: Sender<Header>,
    start_block: u64,
    end_block: u64,
) -> eyre::Result<()> {
    for i in start_block..=end_block {
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Number(i), BlockTransactionsKind::Hashes)
            .await?
            .ok_or_eyre("failed to fetch latest block")?;

        let _ = tx.send(block.header);
    }

    Ok(())
}

async fn fetch_last_blocks(
    provider: &RootProvider,
    tx: Sender<Block>,
    start_block: u64,
    end_block: u64,
) -> eyre::Result<()> {
    for i in start_block..=end_block {
        let block = provider
            .get_block_by_number(BlockNumberOrTag::Number(i), BlockTransactionsKind::Full)
            .await?
            .ok_or_eyre("failed to fetch latest block")?;

        let _ = tx.send(block);
    }

    Ok(())
}

async fn subscribe_headers(ws_url: Url, tx: Sender<Header>) -> eyre::Result<()> {
    info!(ws_url = %ws_url, "subscribing to headers");

    let provider =
        ProviderBuilder::new().disable_recommended_fillers().on_ws(WsConnect::new(ws_url)).await?;
    let mut sub = provider.subscribe_blocks().await?;
    while let Ok(header) = sub.recv().await {
        let _ = tx.send(header);
    }

    Ok(())
}

async fn subscribe_blocks(rpc_url: Url, ws_url: Url, tx: Sender<Block>) -> eyre::Result<()> {
    info!(ws_url = %ws_url, "subscribing to blocks");

    let http_provider = ProviderBuilder::new().disable_recommended_fillers().on_http(rpc_url);
    let ws_provider =
        ProviderBuilder::new().disable_recommended_fillers().on_ws(WsConnect::new(ws_url)).await?;
    let mut sub = ws_provider.subscribe_blocks().await?;
    while let Ok(header) = sub.recv().await {
        let block = http_provider
            .get_block_by_number(
                BlockNumberOrTag::Number(header.number),
                BlockTransactionsKind::Full,
            )
            .await?
            .ok_or_eyre("missing block")?;
        let _ = tx.send(block);
    }

    Ok(())
}

/// Uses taiko_headL1Origin to fetch non preconf blocks
async fn fetch_origin_blocks(
    url: Url,
    tx: Sender<Header>,
    l2_origin: Arc<AtomicU64>,
) -> eyre::Result<()> {
    let provider = ProviderBuilder::new().disable_recommended_fillers().on_http(url);

    let l2_new_origin =
        provider.client().request_noparams::<L1Origin>("taiko_headL1Origin").await?.block_number();

    l2_origin.store(l2_new_origin, Ordering::Relaxed);

    loop {
        let l2_new_origin = provider
            .client()
            .request_noparams::<L1Origin>("taiko_headL1Origin")
            .await?
            .block_number();

        let last = l2_origin.swap(l2_new_origin, Ordering::Relaxed);

        debug!(last, new = l2_new_origin, "fetched l2 origin");

        if l2_new_origin > last {
            BlocksMetrics::l2_origin_block_number(l2_new_origin);
            // fetch all the proposed blocks
            fetch_last_headers(&provider, tx.clone(), last + 1, l2_new_origin).await?;
        }

        sleep(Duration::from_secs(6)).await;
    }
}

#[derive(Debug, Deserialize)]
struct L1Origin {
    #[serde(rename = "blockID")]
    block_id: U256,
}

impl L1Origin {
    fn block_number(&self) -> u64 {
        self.block_id.try_into().unwrap()
    }
}
