use std::time::Duration;

use alloy_eips::BlockNumberOrTag;
use alloy_provider::{Provider, ProviderBuilder, RootProvider, WsConnect};
use alloy_rpc_types::{BlockTransactionsKind, Header};
use crossbeam_channel::Sender;
use eyre::OptionExt;
use reqwest::Client;
use tokio::time::sleep;
use tracing::{error, info};
use url::Url;

/// Fetches new blocks from rpc
// TODO: fix error flows
pub struct BlockFetcher {
    rpc_url: Url,
    ws_url: Url,
    /// Sender for new block updates
    new_block_tx: Sender<Header>,
}

impl BlockFetcher {
    pub fn new(rpc_url: Url, ws_url: Url, new_block_tx: Sender<Header>) -> Self {
        Self { rpc_url, ws_url, new_block_tx }
    }

    #[tracing::instrument(skip_all, name = "block_fetch" fields(id = %id))]
    pub async fn run(self, id: &str, fetch_last: bool) {
        info!(source = %self.rpc_url, ws_url = %self.ws_url, "starting block fetch");

        let backoff = 4;

        while let Err(err) =
            fetch_last_blocks(self.rpc_url.clone(), self.new_block_tx.clone(), fetch_last).await
        {
            error!(%err, backoff, "block fetch loop failed. Retrying..");
            sleep(Duration::from_secs(backoff)).await;
        }

        let safe_lag = if fetch_last { SAFE_LAG_BLOCKS } else { 0 };

        loop {
            if let Err(err) = subscribe_blocks(
                self.rpc_url.clone(),
                self.ws_url.clone(),
                safe_lag,
                self.new_block_tx.clone(),
            )
            .await
            {
                error!(%err, backoff, "block fetch loop failed. Retrying..");
            }

            sleep(Duration::from_secs(backoff)).await;
        }
    }
}

const SAFE_LAG_BLOCKS: u64 = 4;

async fn fetch_last_blocks(url: Url, tx: Sender<Header>, fetch_prev: bool) -> eyre::Result<()> {
    let provider = ProviderBuilder::new().on_http(url.clone());

    let block = provider
        .get_block_by_number(BlockNumberOrTag::Latest, BlockTransactionsKind::Hashes)
        .await?
        .ok_or_eyre("failed to fetch latest block")?;

    if fetch_prev {
        let prev_block = provider
            .get_block_by_number(
                BlockNumberOrTag::Number(block.header.number.saturating_sub(1)),
                BlockTransactionsKind::Hashes,
            )
            .await?
            .ok_or_eyre("failed to fetch latest block")?;

        let _ = tx.send(prev_block.header);
    }

    let _ = tx.send(block.header);

    Ok(())
}

async fn fetch_one_block(
    provider: &RootProvider<alloy_transport_http::Http<Client>>,
    number: BlockNumberOrTag,
) -> eyre::Result<Header> {
    let block = provider.get_block_by_number(number, BlockTransactionsKind::Hashes).await?.unwrap();

    Ok(block.header)
}

async fn subscribe_blocks(
    rpc_url: Url,
    ws_url: Url,
    safe_lag: u64,
    tx: Sender<Header>,
) -> eyre::Result<()> {
    info!(ws_url = %ws_url, rpc_url = %rpc_url, safe_lag, "subscribing to blocks");

    let http_provider = ProviderBuilder::new().on_http(rpc_url);
    let provider = ProviderBuilder::new().on_ws(WsConnect::new(ws_url)).await?;

    let mut sub = provider.subscribe_blocks().await?;

    while let Ok(header) = sub.recv().await {
        if safe_lag == 0 {
            let _ = tx.send(header);
        } else {
            let block_number = header.number.saturating_sub(safe_lag);
            let safe_block =
                fetch_one_block(&http_provider, BlockNumberOrTag::Number(block_number)).await?;
            let _ = tx.send(safe_block);
        }
    }

    Ok(())
}
