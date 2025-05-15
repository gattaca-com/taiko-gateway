#![allow(clippy::comparison_chain)]

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_consensus::{BlobTransactionSidecar, Transaction};
use alloy_eips::BlockId;
use alloy_primitives::Bytes;
use alloy_provider::Provider;
use alloy_rpc_types::{Block, BlockTransactionsKind};
use alloy_sol_types::SolCall;
use eyre::{bail, ensure, eyre, OptionExt};
use pc_common::{
    beacon::BeaconHandle,
    config::{ProposerConfig, TaikoChainParams, TaikoConfig},
    metrics::ProposerMetrics,
    proposer::{
        set_propose_delayed, set_propose_ok, ProposalRequest, ProposeBatchParams, TARGET_BATCH_SIZE,
    },
    sequencer::Order,
    taiko::{
        pacaya::{
            encode_and_compress_orders, l1::TaikoL1::TaikoL1Errors, l2::TaikoL2,
            preconf::PreconfRouter::PreconfRouterErrors, propose_batch_blobs,
            propose_batch_calldata, BlockParams, RevertReason,
        },
        GOLDEN_TOUCH_ADDRESS,
    },
    utils::{alert_discord, verify_and_log_block},
};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, error, info, warn};

use super::L1Client;

type AlloyProvider = alloy_provider::RootProvider;

pub struct ProposerManager {
    config: ProposerConfig,
    client: L1Client,
    l2_provider: AlloyProvider,
    taiko_config: TaikoConfig,
    new_blocks_rx: UnboundedReceiver<ProposalRequest>,
    safe_l1_lag: u64,
    beacon_handle: BeaconHandle,
}

impl ProposerManager {
    pub fn new(
        config: ProposerConfig,
        client: L1Client,
        new_blocks_rx: UnboundedReceiver<ProposalRequest>,
        l2_provider: AlloyProvider,
        taiko_config: TaikoConfig,
        safe_l1_lag: u64,
        beacon_handle: BeaconHandle,
    ) -> Self {
        Self {
            config,
            client,
            new_blocks_rx,
            l2_provider,
            taiko_config,
            safe_l1_lag,
            beacon_handle,
        }
    }

    #[tracing::instrument(skip_all, name = "proposer")]
    pub async fn run(mut self) {
        info!(proposer_address = %self.client.address(), "starting l1 proposer");

        while let Some(request) = self.new_blocks_rx.recv().await {
            match request {
                ProposalRequest::Batch(request) => {
                    info!(
                        n_blocks = request.block_params.len(),
                        all_txs = request.all_tx_list.len(),
                        est_batch_size = request.compressed_est,
                        "proposing batch for blocks {}-{}",
                        request.start_block_num,
                        request.end_block_num
                    );
                    self.propose_batch_with_retry(request).await;
                }
                ProposalRequest::Resync { origin, end } => {
                    while let Err(err) = self.resync(origin, end).await {
                        error!("failed to resync: {err}");
                        tokio::time::sleep(Duration::from_secs(12)).await;
                    }
                }
            }
        }
    }

    #[tracing::instrument(skip_all, name = "resync")]
    pub async fn resync(&self, origin: u64, end: u64) -> eyre::Result<()> {
        info!(origin, end, "resync");

        let blocks = fetch_n_blocks(&self.l2_provider, origin + 1, end).await?;

        let mut to_propose: BTreeMap<u64, Vec<Arc<Block>>> = BTreeMap::new();

        for block in blocks {
            let bn = block.header.number;

            let anchor_tx = block
                .transactions
                .as_transactions()
                .and_then(|txs| txs.first())
                .ok_or_eyre(format!("missing anchor tx in block {bn}"))?;

            ensure!(
                anchor_tx.from == GOLDEN_TOUCH_ADDRESS,
                "expected anchor tx to be from golden touch!"
            );

            let anchor_data = TaikoL2::anchorV3Call::abi_decode(anchor_tx.input(), true)?;
            let anchor_block_id = anchor_data._anchorBlockId;
            to_propose.entry(anchor_block_id).or_default().push(block);
        }

        let l1_block = self
            .client
            .provider()
            .get_block(BlockId::latest(), BlockTransactionsKind::Hashes)
            .await?
            .ok_or_eyre("missing last block")?;

        let l1_head = l1_block.header.number;

        // the parent of the first block in the first batch
        let l2_parent = self
            .l2_provider
            .get_block(BlockId::number(origin), BlockTransactionsKind::Full)
            .await?
            .ok_or_eyre("missing last block")?;

        let mut l2_parent_timestamp = l2_parent.header.timestamp;
        let l2_parent_anchor = l2_parent
            .transactions
            .as_transactions()
            .and_then(|txs| txs.first())
            .ok_or_eyre(format!("missing anchor tx in block {origin}"))?;

        ensure!(
            l2_parent_anchor.from == GOLDEN_TOUCH_ADDRESS,
            "expected anchor tx to be from golden touch!"
        );

        let mut l2_parent_anchor_block_id =
            TaikoL2::anchorV3Call::abi_decode(l2_parent_anchor.input(), true)?._anchorBlockId;

        // check if any batch will reorg
        let mut will_reorg = false;

        for (anchor_block_id, blocks) in to_propose.iter() {
            let start_block = blocks[0].header.number;
            let end_block = blocks[blocks.len() - 1].header.number;

            let l1_timestamp =
                self.beacon_handle.timestamp_of_slot(self.beacon_handle.current_slot() + 1);
            if let Err(err) = validate_batch_params(
                l2_parent_anchor_block_id,
                l2_parent_timestamp,
                l1_head + 1, // assume we land next block
                l1_timestamp,
                *anchor_block_id,
                blocks,
                &self.taiko_config.params,
            ) {
                let msg = format!("batch will reorg: blocks={start_block}-{end_block}, reason={err}, anchor_block_id={anchor_block_id}");
                warn!("{msg}");
                alert_discord(&msg);
                will_reorg = true;
            }

            // parent params for next batch (incorrect if reorg)
            l2_parent_anchor_block_id = *anchor_block_id;
            l2_parent_timestamp = blocks[blocks.len() - 1].header.timestamp;
        }

        if will_reorg {
            // if any batch reorgs, re-anchor all batches and try to propose in as few txs as
            // possible
            let all_blocks = to_propose.into_values().flatten().collect::<Vec<_>>();
            self.propose_reorged_batch(l1_head, all_blocks).await?;
        } else {
            // otherwise propose one batch at a time
            let mut to_verify = BTreeMap::new();

            for (anchor_block_id, blocks) in to_propose {
                let start_block = blocks[0].header.number;
                let end_block = blocks[blocks.len() - 1].header.number;

                info!(anchor_block_id, start_block, end_block, "no reorg");

                for block in blocks.iter() {
                    to_verify.insert(block.header.number, block.clone());
                }

                // propose same batches
                let requests = request_from_blocks(
                    anchor_block_id,
                    blocks,
                    None,
                    None,
                    self.taiko_config.params.max_blocks_per_batch,
                );

                for request in requests {
                    let is_our_coinbase = request.coinbase == self.config.coinbase;
                    self.propose_batch_with_retry(request).await;
                    ProposerMetrics::resync(is_our_coinbase);
                }
            }

            // TODO: this could fetch from preconfs, fix
            for (bn, block) in to_verify {
                let new_block = self
                    .l2_provider
                    .get_block_by_number(bn.into(), BlockTransactionsKind::Hashes)
                    .await?
                    .ok_or(eyre!("missing proposed block {bn}"))?;

                verify_and_log_block(&block.header, &new_block.header, false);
            }
        }

        Ok(())
    }

    async fn propose_reorged_batch(
        &self,
        l1_head: u64,
        blocks: Vec<Arc<Block>>,
    ) -> eyre::Result<()> {
        let safe_l1_head = l1_head.saturating_sub(self.safe_l1_lag);

        let l1_block = self
            .client
            .provider()
            .get_block(BlockId::number(safe_l1_head), BlockTransactionsKind::Hashes)
            .await?
            .ok_or_eyre("missing last block")?;

        // as recent as possible
        let override_timestamp =
            self.beacon_handle.timestamp_of_slot(self.beacon_handle.current_slot() - 1);

        // create a batch proposal with as few txs as possible
        let requests = request_from_blocks(
            l1_block.header.number,
            blocks,
            Some(override_timestamp),
            Some(0),
            self.taiko_config.params.max_blocks_per_batch,
        );

        for request in requests {
            let is_our_coinbase = request.coinbase == self.config.coinbase;
            self.propose_batch_with_retry(request).await;
            ProposerMetrics::resync(is_our_coinbase);
        }

        Ok(())
    }

    async fn prepare_batch(
        &self,
        mut request: ProposeBatchParams,
    ) -> (Bytes, Option<BlobTransactionSidecar>) {
        let parent_meta_hash = self.client.last_meta_hash().await;

        let compressed = encode_and_compress_orders(std::mem::take(&mut request.all_tx_list), true);
        ProposerMetrics::batch_size(compressed.len() as u64);

        if compressed.len() > TARGET_BATCH_SIZE {
            let diff = compressed.len() - TARGET_BATCH_SIZE;
            warn!(
                actual = compressed.len(),
                target = TARGET_BATCH_SIZE,
                diff,
                "exceeding target batch size, is the compression estimate correct?"
            );
        }

        let should_use_blobs = self.should_use_blobs(compressed.len());
        let (input, maybe_sidecar) = if should_use_blobs {
            let (input, sidecar) =
                propose_batch_blobs(request, parent_meta_hash, self.client.address(), compressed);
            (input, Some(sidecar))
        } else {
            let input = propose_batch_calldata(
                request,
                parent_meta_hash,
                self.client.address(),
                compressed,
            );
            (input, None)
        };

        (input, maybe_sidecar)
    }

    #[tracing::instrument(skip_all, name = "propose", fields(start=request.start_block_num, end=request.end_block_num))]
    async fn propose_batch_with_retry(&self, request: ProposeBatchParams) {
        let start_bn = request.start_block_num;
        let end_bn = request.end_block_num;

        debug_assert!(!request.block_params.is_empty(), "no blocks to propose");

        if self.config.dry_run {
            warn!("dry run, skipping proposal");
            return;
        }

        let (input, maybe_sidecar) = self.prepare_batch(request).await;

        const MAX_RETRIES: usize = 3;
        let mut retries = 0;

        while let Err(err) = self.propose_one_batch(input.clone(), maybe_sidecar.clone()).await {
            ProposerMetrics::proposed_batches(false);

            let err_str = err.to_string();
            let err = match RevertReason::try_from(err_str.as_str()) {
                Ok(reason) => match reason {
                    RevertReason::TaikoL1(err) => match err {
                        TaikoL1Errors::TimestampTooLarge(err) => {
                            warn!(?err, "timestamp of last block in batch is > proposal block timestamp, sleeping for 12s");
                            tokio::time::sleep(Duration::from_secs(12)).await;
                            continue;
                        }

                        _ => {
                            format!("unhandled TaikoInbox revert: {err:?}")
                        }
                    },

                    RevertReason::PreconfRouter(err) => match err {
                        PreconfRouterErrors::NotTheOperator(_) => {
                            warn!("failed proposing before change in preconfer, stop retrying");
                            break;
                        }
                        _ => {
                            format!("unhandled PreconfRouter revert: {err:?}")
                        }
                    },

                    RevertReason::PreconfWhitelist(err) => {
                        format!("unhandled PreconfWhitelist revert: {err:?}")
                    }
                },
                Err(_) => err_str,
            };

            set_propose_delayed();
            retries += 1;

            if retries == 1 {
                let msg = format!(
                    "failed to propose batch, retrying in 12 secs. bns={start_bn}-{end_bn}, err={err}"
                );
                error!("{msg}");
                alert_discord(&msg);
            }

            if retries == MAX_RETRIES {
                let msg = format!(
                    "max retries reached to propose batch. bns={start_bn}-{end_bn}, err={err}"
                );
                panic!("{msg}");
            }

            tokio::time::sleep(Duration::from_secs(12)).await;
        }

        ProposerMetrics::proposed_batches(true);

        set_propose_ok();

        if retries > 0 {
            let msg = format!("resolved propose batch, bns={start_bn}-{end_bn}");
            alert_discord(&msg);
        }
    }

    async fn propose_one_batch(
        &self,
        input: Bytes,
        sidecar: Option<BlobTransactionSidecar>,
    ) -> eyre::Result<()> {
        let tx = if let Some(sidecar) = sidecar {
            debug!(blobs = sidecar.blobs.len(), "building blob tx");
            self.client.build_eip4844(input, sidecar).await?
        } else {
            self.client.build_eip1559(input).await?
        };

        info!("type" = %tx.tx_type(), tx_hash = %tx.tx_hash(), "sending blocks proposal tx");

        let start = Instant::now();
        let tx_receipt = self.client.send_tx(tx).await?;
        ProposerMetrics::proposal_latency(start.elapsed());

        let tx_hash = tx_receipt.transaction_hash;
        let block_number = tx_receipt.block_number.unwrap_or_default();

        if tx_receipt.status() {
            info!(
                %tx_hash,
                l1_bn = block_number,
                "proposed batch"
            );

            self.client.reorg_check(tx_hash).await.inspect_err(|_| {
                ProposerMetrics::proposal_reorg();
            })?;

            Ok(())
        } else {
            let receipt_json = serde_json::to_string(&tx_receipt)?;
            bail!("failed to propose block: l1_bn={block_number}, tx_hash={tx_hash}, receipt={receipt_json}");
        }
    }

    // TODO: use gas cost instead of size
    fn should_use_blobs(&self, compressed_size: usize) -> bool {
        const CALLDATA_SIZE: usize = 50_000; // max calldata with some buffer
        compressed_size > CALLDATA_SIZE
    }
}

async fn fetch_n_blocks(
    provider: &AlloyProvider,
    start: u64,
    end: u64,
) -> eyre::Result<Vec<Arc<Block>>> {
    let mut blocks = Vec::new();

    for bn in start..=end {
        let block = provider
            .get_block_by_number(bn.into(), BlockTransactionsKind::Full)
            .await?
            .ok_or_else(|| eyre!("failed to fetch block {bn} from rpc"))?;
        blocks.push(block.into());
    }

    Ok(blocks)
}

fn request_from_blocks(
    anchor_block_id: u64,
    full_blocks: Vec<Arc<Block>>,
    timestamp_override: Option<u64>,
    timeshift_override: Option<u8>,
    max_blocks_per_batch: usize,
) -> Vec<ProposeBatchParams> {
    assert!(!full_blocks.is_empty(), "no blocks to propose");

    let mut batches = Vec::new();

    // assume that all blocks have the same coinbase
    let coinbase = full_blocks[0].header.beneficiary;

    let mut total_time_shift = 0;
    let mut total_txs = 0;

    let mut cur_params = ProposeBatchParams {
        anchor_block_id,
        start_block_num: full_blocks[0].header.number,
        end_block_num: full_blocks[0].header.number,
        block_params: Vec::new(),
        all_tx_list: Vec::new(),
        compressed_est: 0,
        last_timestamp: timestamp_override.unwrap_or(full_blocks[0].header.timestamp),
        coinbase,
    };

    let mut i = 0;
    while i < full_blocks.len() {
        let block = Arc::unwrap_or_clone(full_blocks[i].clone());
        let block_txs = block.transactions.len() - 1; // remove anchor
        total_txs += block_txs;

        let txs = block
            .transactions
            .into_transactions()
            .filter(|tx| tx.from != GOLDEN_TOUCH_ADDRESS)
            .map(|tx| Order::new_with_sender(tx.inner, tx.from))
            .collect::<Vec<_>>();

        let mut new_tx_list = cur_params.all_tx_list.clone();
        new_tx_list.extend(txs.clone());

        let compressed = encode_and_compress_orders(new_tx_list.clone(), false);
        if compressed.len() > TARGET_BATCH_SIZE ||
            cur_params.block_params.len() >= max_blocks_per_batch
        {
            // push previous params and start new one
            let new_params = ProposeBatchParams {
                anchor_block_id,
                start_block_num: block.header.number,
                end_block_num: 0, // set later
                block_params: Vec::new(),
                all_tx_list: txs,
                compressed_est: compressed.len(),
                last_timestamp: timestamp_override.unwrap_or(block.header.timestamp),
                coinbase,
            };

            let old_params = std::mem::replace(&mut cur_params, new_params);
            batches.push(old_params);
        } else {
            cur_params.all_tx_list = new_tx_list;
            cur_params.compressed_est = compressed.len();
        }

        let time_shift: u8 = block
            .header
            .timestamp
            .saturating_sub(cur_params.last_timestamp)
            .try_into()
            .inspect_err(|_| {
                error!(
                    prev_timestamp = cur_params.last_timestamp,
                    block_timestamp = block.header.timestamp,
                    block_number = block.header.number,
                    "resync time shift too large"
                )
            })
            .unwrap_or(0);
        let time_shift = timeshift_override.unwrap_or(time_shift);
        total_time_shift += time_shift as u64;

        cur_params.last_timestamp = timestamp_override.unwrap_or(block.header.timestamp);
        cur_params.end_block_num = block.header.number;

        cur_params.block_params.push(BlockParams {
            numTransactions: block_txs as u16,
            timeShift: time_shift,
            signalSlots: vec![], // TODO
        });

        i += 1;
    }

    batches.push(cur_params);

    for params in batches.iter() {
        debug!(
            est_batch_size = params.compressed_est,
            blocks = params.block_params.len(),
            last_timestamp = params.last_timestamp,
            total_time_shift,
            total_txs,
            %coinbase,
            ?timestamp_override,
            ?timeshift_override,
            "resync request"
        );
    }

    batches
}

// checks similar to _validateBatchParams in TaikoInbox
fn validate_batch_params(
    l2_parent_anchor_block_id: u64,
    l2_parent_timestamp: u64,
    l1_block: u64, // where it will land
    l1_timestamp: u64,
    anchor_block_id: u64,
    blocks: &[Arc<Block>],
    config: &TaikoChainParams,
) -> eyre::Result<()> {
    debug!(
        anchor_block_id,
        l2_parent_anchor_block_id,
        l2_parent_timestamp,
        l1_block,
        l1_timestamp,
        "validating batch params"
    );

    ensure!(!blocks.is_empty(), "no blocks to propose");
    ensure!(blocks.len() <= config.max_blocks_per_batch, "too many blocks to propose");

    ensure!(
        anchor_block_id + config.safe_anchor_height_offset() >= l1_block,
        "anchor block id too small"
    );
    ensure!(anchor_block_id < l1_block, "anchor block id too large");
    ensure!(anchor_block_id >= l2_parent_anchor_block_id, "anchor block id smaller than parent");

    let first_timestamp = blocks[0].header.timestamp;
    let last_timestamp = blocks[blocks.len() - 1].header.timestamp;

    ensure!(
        last_timestamp <= l1_timestamp,
        "timestamp too large {} {}",
        last_timestamp,
        l1_timestamp
    );
    // skip first timeshift == 0

    ensure!(last_timestamp >= first_timestamp, "block timestamps should increase");

    let total_time_shift = last_timestamp - first_timestamp;
    ensure!(last_timestamp >= total_time_shift, "last timestamp too small");

    ensure!(
        first_timestamp + config.safe_anchor_height_offset() * 12 >= l1_timestamp,
        "first timestamp too small"
    );

    ensure!(first_timestamp >= l2_parent_timestamp, "first timestamp smaller than parent");

    // skip metahash check for now

    Ok(())
}
