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
use alloy_rpc_types::{BlockTransactionsKind, Header};
use crossbeam_channel::Sender;
use eyre::OptionExt;
use pc_common::metrics::BlocksMetrics;
use reqwest::Url;
use serde::Deserialize;
use tokio::time::sleep;
use tracing::{error, info};

/// Fetches new blocks from rpc
// TODO: fix error flows
pub struct BlockFetcher {
    url: Url,
    /// Sender for new block updates
    new_block_tx: Sender<Header>,
}

impl BlockFetcher {
    pub fn new(rpc_url: Url, new_block_tx: Sender<Header>) -> Self {
        Self { url: rpc_url, new_block_tx }
    }

    #[tracing::instrument(skip_all, name = "block_fetch" fields(id = %id))]
    pub async fn run_fetch(self, id: &str, ws_url: Url, fetch_last: u64) {
        info!(fetch_last, source = %self.url, %ws_url, "starting block fetch");

        let backoff = 4;

        let provider =
            ProviderBuilder::new().disable_recommended_fillers().on_http(self.url.clone());

        let last_block = provider.get_block_number().await.expect("failed to fetch last block");

        while let Err(err) = fetch_last_blocks(
            &provider,
            self.new_block_tx.clone(),
            last_block - fetch_last,
            last_block,
        )
        .await
        {
            error!(%err, backoff, "block fetch loop failed. Retrying..");
            sleep(Duration::from_secs(backoff)).await;
        }

        loop {
            if let Err(err) = subscribe_blocks(ws_url.clone(), self.new_block_tx.clone()).await {
                error!(%err, backoff, "block fetch loop failed. Retrying..");
            }

            sleep(Duration::from_secs(backoff)).await;
        }
    }

    #[tracing::instrument(skip_all, name = "preconf_fetch" fields(id = %id))]
    pub async fn run_origin_fetch(self, id: &str, l2_origin: Arc<AtomicU64>) {
        info!(source = %self.url, "starting preconf check fetch");

        let backoff = 4;

        loop {
            if let Err(err) =
                fetch_origin_blocks(self.url.clone(), self.new_block_tx.clone(), l2_origin.clone())
                    .await
            {
                error!(%err, backoff, "block fetch loop failed. Retrying..");
            }

            sleep(Duration::from_secs(backoff)).await;
        }
    }
}

async fn fetch_last_blocks(
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

async fn subscribe_blocks(ws_url: Url, tx: Sender<Header>) -> eyre::Result<()> {
    info!(ws_url = %ws_url, "subscribing to blocks");

    let provider =
        ProviderBuilder::new().disable_recommended_fillers().on_ws(WsConnect::new(ws_url)).await?;
    let mut sub = provider.subscribe_blocks().await?;
    while let Ok(header) = sub.recv().await {
        let _ = tx.send(header);
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

        if l2_new_origin > last {
            BlocksMetrics::l2_origin_block_number(l2_new_origin);
            // fetch all the proposed blocks
            fetch_last_blocks(&provider, tx.clone(), last + 1, l2_new_origin).await?;
        }

        sleep(Duration::from_secs(12)).await;
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
