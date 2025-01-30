use std::time::Duration;

use alloy_eips::BlockNumberOrTag;
use alloy_provider::{Provider, ProviderBuilder, WsConnect};
use alloy_rpc_types::{BlockTransactionsKind, Header};
use crossbeam_channel::Sender;
use eyre::OptionExt;
use tokio::time::sleep;
use tracing::{error, info};
use url::Url;

/// Fetches new blocks from rpc
// TODO: fix error flows
pub struct BlockFetcher {
    rpc_url: Url,
    /// Websocket url
    ws_url: Url,
    /// Sender for new block updates
    new_block_tx: Sender<Header>,
}

impl BlockFetcher {
    pub fn new(rpc_url: Url, ws_url: Url, new_block_tx: Sender<Header>) -> Self {
        Self { rpc_url, ws_url, new_block_tx }
    }

    #[tracing::instrument(skip_all, name = "block_fetch" fields(id = %id))]
    pub async fn run(self, id: &str, fetch_last: u64) {
        info!(fetch_last, source = %self.rpc_url, ws_url = %self.ws_url, "starting block fetch");

        let backoff = 4;

        while let Err(err) =
            fetch_last_blocks(self.rpc_url.clone(), self.new_block_tx.clone(), fetch_last).await
        {
            error!(%err, backoff, "block fetch loop failed. Retrying..");
            sleep(Duration::from_secs(backoff)).await;
        }

        loop {
            if let Err(err) = subscribe_blocks(self.ws_url.clone(), self.new_block_tx.clone()).await
            {
                error!(%err, backoff, "block fetch loop failed. Retrying..");
            }

            sleep(Duration::from_secs(backoff)).await;
        }
    }
}

async fn fetch_last_blocks(url: Url, tx: Sender<Header>, fetch_last: u64) -> eyre::Result<()> {
    let provider = ProviderBuilder::new().on_http(url);

    let block = provider
        .get_block_by_number(BlockNumberOrTag::Latest, BlockTransactionsKind::Hashes)
        .await?
        .ok_or_eyre("failed to fetch latest block")?;

    let last_block_number = block.header.number;

    for i in (1..fetch_last).rev() {
        let prev_block = provider
            .get_block_by_number(
                BlockNumberOrTag::Number(last_block_number - i),
                BlockTransactionsKind::Hashes,
            )
            .await?
            .ok_or_eyre("failed to fetch latest block")?;

        let _ = tx.send(prev_block.header);
    }

    let _ = tx.send(block.header);

    Ok(())
}

async fn subscribe_blocks(ws_url: Url, tx: Sender<Header>) -> eyre::Result<()> {
    info!(ws_url = %ws_url, "subscribing to blocks");

    let provider = ProviderBuilder::new().on_ws(WsConnect::new(ws_url)).await?;
    let mut sub = provider.subscribe_blocks().await?;
    while let Ok(header) = sub.recv().await {
        let _ = tx.send(header);
    }

    Ok(())
}
