use std::{
    collections::HashMap,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_consensus::Transaction;
use alloy_eips::BlockNumberOrTag;
use alloy_primitives::{BlockHash, B256};
use alloy_provider::{network::primitives::BlockTransactionsKind, Provider, ProviderBuilder};
use alloy_sol_types::SolCall;
use eyre::{bail, ensure, eyre, OptionExt};
use pc_common::{
    config::{ProposerConfig, TaikoChainConfig},
    metrics::GatewayMetrics,
    proposer::ProposerRequest,
    sequencer::SequenceLoop,
    taiko::{l2::TaikoL2::anchorV2Call, GOLDEN_TOUCH_ADDRESS},
    types::DuplexChannel,
    utils::{alert_discord, verify_and_log_block},
};
use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender};
use tracing::{error, info};
use url::Url;

use crate::{includer::L1Includer, types::Propose};

pub struct Proposer {
    continue_loop_time: Duration,

    block_tx: UnboundedSender<ProposerRequest>,
    to_sequencer: DuplexChannel<SequenceLoop, ProposerRequest>,
}

impl Proposer {
    pub fn new(
        config: ProposerConfig,
        block_tx: UnboundedSender<ProposerRequest>,
        to_sequencer: DuplexChannel<SequenceLoop, ProposerRequest>,
    ) -> Self {
        Self { continue_loop_time: config.continue_loop_time, block_tx, to_sequencer }
    }

    pub fn run(self) {
        self.send_continue_loop();

        let continue_tick = crossbeam_channel::tick(self.continue_loop_time / 2);

        loop {
            crossbeam_channel::select! {
                recv(continue_tick) -> _ => {
                    self.send_continue_loop();
                }

                recv(self.to_sequencer.rx) -> maybe_request => {
                    match maybe_request {
                        Ok(request) => {
                            self.block_tx.send(request).unwrap();
                        },

                        Err(err) => error!(%err, "proposer_rx is closed")
                    }
                }
            }
        }
    }

    fn send_continue_loop(&self) {
        let stop_by = Instant::now() + self.continue_loop_time;

        if let Err(err) = self.to_sequencer.tx.send(SequenceLoop::Continue { stop_by }) {
            error!(%err, "failed to send continue loop signal");
            // TODO: this means sequencer is down, we should do a graceful exit after all the
            // pending blocks are proposed
        }
    }
}

/// If we restart and some blocks were not proposed, fetch from preconf tip and re-send proposals
#[allow(clippy::comparison_chain)]
#[tracing::instrument(skip_all, name = "resync", fields(force_reorgs = force_reorgs))]
pub async fn resync_blocks(
    l1_url: Url,
    preconf_url: Url,
    chain_url: Url,
    chain_config: TaikoChainConfig,
    proposer: Arc<Propose>,
    includer: Arc<L1Includer>,
    force_reorgs: bool,
) -> eyre::Result<()> {
    let l1_provider = ProviderBuilder::new().with_recommended_fillers().on_http(l1_url);
    let chain_provider = ProviderBuilder::new().with_recommended_fillers().on_http(chain_url);
    let preconf_provider = ProviderBuilder::new().with_recommended_fillers().on_http(preconf_url);

    let mut chain_head = chain_provider.get_block_number().await?;
    let mut preconf_head = preconf_provider.get_block_number().await?;

    info!(chain_head, preconf_head, "resyncing preconf blocks");

    if chain_head > preconf_head {
        // TODO: this can happen in a edge case where we a block has just been proposed and
        // simulator hasn't synced yet
        bail!("simulator is behind chain, this should not happen")
    } else if chain_head < preconf_head {
        while chain_head < preconf_head {
            let mut requests = vec![];

            let l1_head = l1_provider
                .get_block_by_number(BlockNumberOrTag::Latest, BlockTransactionsKind::Hashes)
                .await?
                .ok_or_eyre("failed l1 fetch")?;

            let mut to_verify = HashMap::new();
            let mut has_reorged = false;

            // sync max 10 blocks at a time
            let max_preconf_bn = preconf_head.min(chain_head + 1 + 10);
            for bn in (chain_head + 1)..=max_preconf_bn {
                let block = preconf_provider
                    .get_block_by_number(bn.into(), BlockTransactionsKind::Full)
                    .await?
                    .ok_or_eyre("failed to get block")?;
                let block = Arc::new(block);
                to_verify.insert(bn, block.clone());

                // unwraps are ok because hydrate = true and there's at least one tx
                let anchor_tx = block.transactions.as_transactions().unwrap().first().unwrap();

                assert_eq!(
                    anchor_tx.from, GOLDEN_TOUCH_ADDRESS,
                    "expected first tx to be from anchor"
                );

                // get the anchor data
                let anchor_data = anchorV2Call::abi_decode(&anchor_tx.input(), true)?;
                let anchor_block_id = anchor_data._anchorBlockId;
                let anchor_timestamp = block.header.timestamp;

                let reorg_by_number = l1_head.header.number.saturating_sub(anchor_block_id) >=
                    chain_config.max_anchor_height_offset;
                let reorg_by_timestamp = l1_head.header.timestamp.saturating_sub(anchor_timestamp) >=
                    chain_config.max_anchor_timestamp_offset;

                if reorg_by_number || reorg_by_timestamp {
                    // this can happen if we waited too long before restarting the proposer, a
                    // re-org is inevitable
                    if force_reorgs {
                        let request = ProposerRequest {
                            block,
                            anchor_block_id: l1_head.header.number - 1,
                            anchor_timestamp: l1_head.header.timestamp - 12,
                        };
                        requests.push(request);
                        has_reorged = true;
                    } else {
                        panic!("unrecoverable re-org (number), check manually");
                    }
                } else {
                    // propose same block
                    let request = ProposerRequest { block, anchor_block_id, anchor_timestamp };
                    requests.push(request);
                }
            }

            if has_reorged {
                let msg = format!("re-orged blocks {}-{}", chain_head + 1, max_preconf_bn);
                alert_discord(&msg);
            }

            propose_blocks(requests, &proposer, &includer, false).await?;

            if !has_reorged {
                for bn in (chain_head + 1)..=max_preconf_bn {
                    let preconf_block = to_verify.remove(&bn).unwrap();
                    let new_block = chain_provider
                        .get_block_by_number(bn.into(), BlockTransactionsKind::Hashes)
                        .await?
                        .ok_or_eyre("failed to get chain block")?;

                    verify_and_log_block(&preconf_block.header, &new_block.header, true);
                }
            }

            chain_head = chain_provider.get_block_number().await?;
            preconf_head = preconf_provider.get_block_number().await?;
        }
    }

    Ok(())
}

// FIXME
#[tracing::instrument(skip_all, name = "proposer")]
pub async fn propose_pending_blocks(
    mut block_rx: UnboundedReceiver<ProposerRequest>,
    proposer: Arc<Propose>,
    includer: Arc<L1Includer>,
    use_blobs: bool,
    use_batch: bool,
) {
    if use_batch {
        let mut requests = vec![];
        loop {
            block_rx.recv_many(&mut requests, 10).await;
            info!(pending = requests.len(), "new pending blocks");

            if requests.len() == 1 {
                propose_block_forever(&requests[0], &proposer, &includer, use_blobs).await;
            } else if !requests.is_empty() {
                propose_blocks_forever(&requests, &proposer, &includer, use_blobs).await;
            }

            requests.clear();
        }
    } else {
        while let Some(block) = block_rx.recv().await {
            propose_block_forever(&block, &proposer, &includer, use_blobs).await;
        }
    }
}

const MAX_RETRIES: usize = 4;

async fn propose_block_forever(
    request: &ProposerRequest,
    proposer: &Arc<Propose>,
    includer: &L1Includer,
    use_blobs: bool,
) {
    let mut retries = 0;
    while let Err(err) = propose_one_block(request.clone(), proposer, includer, use_blobs).await {
        error!(%err, "failed to propose block, retrying in 12 secs");
        GatewayMetrics::record_proposer_status_code(false);
        tokio::time::sleep(Duration::from_secs(12)).await;

        retries += 1;

        if retries == 1 {
            let msg = format!("failed to propose block={}, err={}", request.block.header.hash, err);
            alert_discord(&msg);
        }

        if retries == MAX_RETRIES {
            let msg = format!(
                "max retries reached to propose block={}, err={}",
                request.block.header.hash, err
            );
            panic!("{}", msg);
        }
    }

    GatewayMetrics::record_proposer_status_code(true);
    if retries > 0 {
        let msg = format!("resolved propose block={}", request.block.header.hash);
        alert_discord(&msg);
    }
}

async fn propose_blocks_forever(
    requests: &Vec<ProposerRequest>,
    proposer: &Arc<Propose>,
    includer: &L1Includer,
    use_blobs: bool,
) {
    let mut retries = 0;
    let hashes: Vec<BlockHash> = requests.iter().map(|block| block.block.header.hash).collect();
    while let Err(err) = propose_blocks(requests.clone(), proposer, includer, use_blobs).await {
        error!(%err, "failed to propose block, retrying in 12 secs");
        GatewayMetrics::record_proposer_status_code(false);

        tokio::time::sleep(Duration::from_secs(12)).await;
        retries += 1;

        if retries == 1 {
            let msg = format!("failed to propose blocks={:?}, err={}", hashes, err);
            // TODO: Make this async
            alert_discord(&msg);
        }

        if retries == MAX_RETRIES {
            let msg = format!("max retries reached to propose blocks={:?}, err={}", hashes, err);
            panic!("{}", msg);
        }
    }

    GatewayMetrics::record_proposer_status_code(true);
    if retries > 0 {
        let msg = format!("resolved propose blocks={:?}", hashes);
        alert_discord(&msg);
    }
}

#[tracing::instrument(skip_all, fields(use_blobs = use_blobs, n_blocks = requests.len()))]
async fn propose_blocks(
    requests: Vec<ProposerRequest>,
    proposer: &Arc<Propose>,
    includer: &L1Includer,
    use_blobs: bool,
) -> eyre::Result<()> {
    ensure!(!requests.is_empty(), "request cannot be empty");

    let n_txs: usize = requests.iter().map(|req| req.block.transactions.len()).sum();
    let block_hashes: Vec<B256> = requests.iter().map(|req| req.block.header.hash).collect();

    let tx = if use_blobs {
        let fields = proposer
            .get_propose_txs_blob(requests)
            .await
            .map_err(|err| eyre!("propose tx blob {err}"))?;
        includer.generate_blob_tx(fields).await.map_err(|err| eyre!("generate blob {err}"))?
    } else {
        let fields = proposer
            .get_propose_txs_calldata(requests)
            .await
            .map_err(|err| eyre!("propose tx calldata {err}"))?;
        includer.generate_eip1559_tx(fields).await.map_err(|err| eyre!("generate eip559 {err}"))?
    };

    info!(tx_hash = %tx.tx_hash(), n_txs, ?block_hashes, "sending blocks proposal tx");
    let tx_receipt = includer.send_tx(tx).await.map_err(|err| eyre!("send blob {err:?}"))?;

    let tx_hash = tx_receipt.transaction_hash;
    let block_number = tx_receipt.block_number.unwrap_or_default();

    if tx_receipt.status() {
        info!(
            %tx_hash,
            l1_bn = block_number,
            "proposed blocks"
        );

        reorg_check(tx_hash, includer).await?;

        Ok(())
    } else {
        let receipt_json = serde_json::to_string(&tx_receipt)?;
        // TODO: check error type and return different errors so we can recover or not
        bail!("failed to propose block: l1_bn={block_number}, tx_hash={tx_hash}, receipt={receipt_json}");
    }
}

#[tracing::instrument(skip_all, name = "propose_block", fields(l2_hash = %request.block.header.hash, l2_bn = request.block.header.number, use_blobs = use_blobs))]
async fn propose_one_block(
    request: ProposerRequest,
    proposer: &Arc<Propose>,
    includer: &L1Includer,
    use_blobs: bool,
) -> eyre::Result<()> {
    let block = request.block.clone();

    info!(
        anchor_block_id = request.anchor_block_id,
        anchor_ts = request.anchor_timestamp,
        "proposing block"
    );

    let tx = if use_blobs {
        let blob_fields = proposer
            .get_propose_tx_blob(request)
            .await
            .map_err(|err| eyre!("propose tx blob: {err}"))?;

        includer.generate_blob_tx(blob_fields).await.map_err(|err| eyre!("generate blob: {err}"))?
    } else {
        let calldata_fields = proposer
            .get_propose_tx_calldata(request)
            .await
            .map_err(|err| eyre!("propose tx calldata: {err}"))?;

        includer
            .generate_eip1559_tx(calldata_fields)
            .await
            .map_err(|err| eyre!("generate eip559: {err}"))?
    };

    info!(tx_hash = %tx.tx_hash(), n_txs = block.transactions.len().saturating_sub(1), "sending block proposal tx");
    let tx_receipt = includer.send_tx(tx).await.map_err(|err| eyre!("send blob {err:?}"))?;

    let tx_hash = tx_receipt.transaction_hash;
    let block_number = tx_receipt.block_number.ok_or_eyre("tx not included in a block")?;

    if tx_receipt.status() {
        info!(
            %tx_hash,
            l1_bn = block_number,
            "proposed block"
        );

        reorg_check(tx_hash, includer).await?;

        Ok(())
    } else {
        let receipt_json = serde_json::to_string(&tx_receipt)?;
        // TODO: check error type and return different errors so we can recover
        bail!("failed to propose block: l1_bn={block_number}, tx_hash={tx_hash}, receipt={receipt_json}");
    }
}

// 3 blocks
const REORG_SAFE_TIME_SECS: u64 = 12 * 3;

/// Tx hash is for a tx that was just included in a block and we need to make sure it will not be
/// re-orged out
#[tracing::instrument(skip_all, name = "reorg_check", fields(tx_hash = %tx_hash))]
async fn reorg_check(tx_hash: B256, includer: &L1Includer) -> eyre::Result<()> {
    info!("starting reorg check");

    // after this time it's extremely unlikely that there's a re-org
    tokio::time::sleep(Duration::from_secs(REORG_SAFE_TIME_SECS)).await;

    let tx_receipt = includer
        .provider()
        .get_transaction_receipt(tx_hash)
        .await?
        .ok_or_eyre("tx not found, block was likely re-orged")?;

    ensure!(tx_receipt.block_number.is_some(), "tx was re-orged out");

    info!("check successful");

    Ok(())
}
