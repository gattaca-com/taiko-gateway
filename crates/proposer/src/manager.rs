#![allow(clippy::comparison_chain)]

use std::{
    collections::BTreeMap,
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_consensus::{BlobTransactionSidecar, Transaction};
use alloy_eips::{eip4844::env_settings::EnvKzgSettings, BlockId, Encodable2718};
use alloy_network::TransactionResponse;
use alloy_primitives::{hex, Bytes, B256};
use alloy_provider::Provider;
use alloy_rpc_types::Block;
use alloy_sol_types::SolCall;
use eyre::{bail, ensure, eyre, OptionExt};
use pc_common::{
    beacon::BeaconHandle,
    config::{ProposerConfig, TaikoChainParams, TaikoConfig},
    metrics::ProposerMetrics,
    proposer::{
        set_propose_delayed, set_propose_ok, LivePending, ProposalRequest, ProposeBatchParams,
    },
    runtime::spawn,
    sequencer::Order,
    taiko::{
        lookahead::LookaheadHandle,
        pacaya::{
            encode_and_compress_orders, l1::TaikoL1::TaikoL1Errors, l2::TaikoL2,
            preconf::PreconfRouter::PreconfRouterErrors, propose_batch_blobs,
            propose_batch_calldata, BlockParams, RevertReason,
        },
        GOLDEN_TOUCH_ADDRESS,
    },
    types::FailReason,
    utils::{alert_discord, verify_and_log_block},
};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{debug, error, info, warn};

use super::L1Client;
use crate::{builder::send_bundle_request, error::ProposerError};

type AlloyProvider = alloy_provider::RootProvider;

#[derive(Copy, Clone)]
pub struct PendingProposal {
    // both inclusive
    pub start_block_num: u64,
    pub end_block_num: u64,
    pub anchor_block_id: u64,
    pub nonce: u64,
    pub tx_hash: B256,
}

impl std::fmt::Debug for PendingProposal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "PendingProposal(blocks: {}-{}, anchor: {}, nonce: {}, tx_hash: {})",
            self.start_block_num,
            self.end_block_num,
            self.anchor_block_id,
            self.nonce,
            self.tx_hash
        )
    }
}

#[derive(Clone)]
pub struct ProposerManager {
    config: Arc<ProposerConfig>,
    client: Arc<L1Client>,
    tx_send_provider: Option<AlloyProvider>,
    l2_provider: Arc<AlloyProvider>,
    taiko_config: Arc<TaikoConfig>,
    safe_l1_lag: u64,
    beacon_handle: BeaconHandle,
    lookahead: Arc<LookaheadHandle>,
}

impl ProposerManager {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        config: ProposerConfig,
        client: L1Client,
        tx_send_provider: Option<AlloyProvider>,
        l2_provider: AlloyProvider,
        taiko_config: TaikoConfig,
        safe_l1_lag: u64,
        beacon_handle: BeaconHandle,
        lookahead: Arc<LookaheadHandle>,
    ) -> Self {
        Self {
            config: config.into(),
            client: client.into(),
            tx_send_provider,
            l2_provider: l2_provider.into(),
            taiko_config: taiko_config.into(),
            safe_l1_lag,
            beacon_handle,
            lookahead,
        }
    }

    #[tracing::instrument(skip_all, name = "proposer")]
    pub async fn run(self, mut new_blocks_rx: UnboundedReceiver<ProposalRequest>) {
        info!(proposer_address = %self.client.address(), "starting l1 proposer");

        match self.client.get_pending_proposals().await {
            Ok(pending) => {
                info!(n_pending = pending.len(), ?pending, "fetched pending proposals");
            }
            Err(err) => {
                error!(%err, "failed to fetch pending proposals");
            }
        };

        while let Some(request) = new_blocks_rx.recv().await {
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
                    self.spawn_propose_batch_with_retry(request).await;
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
        let all_pending = match self.client.get_pending_proposals().await {
            Ok(pending) => {
                info!(n_pending = pending.len(), ?pending, "fetched pending proposals");
                pending
            }
            Err(err) => {
                error!(%err, "failed to fetch pending proposals");
                vec![]
            }
        };

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
                anchor_tx.from() == GOLDEN_TOUCH_ADDRESS,
                "expected anchor tx to be from golden touch!"
            );

            let anchor_data = TaikoL2::anchorV3Call::abi_decode(anchor_tx.input())?;
            let anchor_block_id = anchor_data._anchorBlockId;
            to_propose.entry(anchor_block_id).or_default().push(block);
        }

        let l1_block = self
            .client
            .provider()
            .get_block(BlockId::latest())
            .await?
            .ok_or_eyre("missing last block")?;

        let l1_head = l1_block.header.number;

        // the parent of the first block in the first batch
        let l2_parent = self
            .l2_provider
            .get_block(BlockId::number(origin))
            .full()
            .await?
            .ok_or_eyre("missing last block")?;

        let mut l2_parent_timestamp = l2_parent.header.timestamp;
        let l2_parent_anchor = l2_parent
            .transactions
            .as_transactions()
            .and_then(|txs| txs.first())
            .ok_or_eyre(format!("missing anchor tx in block {origin}"))?;

        ensure!(
            l2_parent_anchor.from() == GOLDEN_TOUCH_ADDRESS,
            "expected anchor tx to be from golden touch!"
        );

        let mut l2_parent_anchor_block_id =
            TaikoL2::anchorV3Call::abi_decode(l2_parent_anchor.input())?._anchorBlockId;

        // check if any batch will reorg
        let mut will_reorg = false;

        for (anchor_block_id, blocks) in to_propose.iter() {
            let start_block = blocks[0].header.number;
            let end_block = blocks[blocks.len() - 1].header.number;

            // TODO: we could do something more here, eg check batches overlaps. For now dont make
            // it too complex and only check for the exact same batch
            if let Some(pending) = all_pending.iter().find(|p| {
                p.anchor_block_id == *anchor_block_id &&
                    p.start_block_num == start_block &&
                    p.end_block_num == end_block
            }) {
                warn!(
                    nonce = pending.nonce,
                    hash =% pending.tx_hash,
                    "batch already pending, skipping resync"
                );
                continue;
            }

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
                    self.config.batch_target_size,
                );

                LivePending::add_n_pending(requests.len());
                debug!("resyncing {} batches", requests.len());

                for request in requests {
                    let is_our_coinbase = request.coinbase == self.config.coinbase;
                    self.spawn_propose_batch_with_retry(request).await;
                    ProposerMetrics::resync(is_our_coinbase);
                }
            }

            // TODO: this could fetch from preconfs, fix
            for (bn, block) in to_verify {
                let new_block = self
                    .l2_provider
                    .get_block_by_number(bn.into())
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
            .get_block(BlockId::number(safe_l1_head))
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
            self.config.batch_target_size,
        );

        LivePending::add_n_pending(requests.len());
        debug!("reorg-resyncing {} batches", requests.len());

        for request in requests {
            let is_our_coinbase = request.coinbase == self.config.coinbase;
            self.spawn_propose_batch_with_retry(request).await;
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

        if compressed.len() > self.config.batch_target_size {
            let diff = compressed.len() - self.config.batch_target_size;
            warn!(
                actual = compressed.len(),
                target = self.config.batch_target_size,
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

    async fn spawn_propose_batch_with_retry(&self, request: ProposeBatchParams) {
        info!(
            live_pending = LivePending::current(),
            start_bn = request.start_block_num,
            end_bn = request.end_block_num,
            blocks = request.block_params.len(),
            "spawning propose batch"
        );
        self.propose_batch(request).await
    }

    #[tracing::instrument(skip_all, name = "propose", fields(start=request.start_block_num, end=request.end_block_num))]
    async fn propose_batch(&self, request: ProposeBatchParams) {
        let start_bn = request.start_block_num;
        let end_bn = request.end_block_num;
        let block_count = request.block_params.len();

        debug_assert!(!request.block_params.is_empty(), "no blocks to propose");

        if self.config.dry_run {
            warn!("dry run, skipping proposal");
            return;
        }

        let (input, maybe_sidecar) = self.prepare_batch(request).await;

        const MAX_RETRIES: usize = 3;
        let mut retries = 0;
        let mut bump_fees = false;
        let mut last_used_nonce = u64::MAX;
        let mut tip_cap = None;
        let mut blob_fee_cap = None;
        let mut gas_fee_cap = None;

        while let Err(err) = self
            .propose_one_batch(
                input.clone(),
                maybe_sidecar.clone(),
                bump_fees,
                tip_cap,
                blob_fee_cap,
                gas_fee_cap,
                &mut last_used_nonce,
            )
            .await
        {
            if let Some(err) = err.downcast_ref::<ProposerError>() {
                match err {
                    ProposerError::TxNotIncludedInNextBlocks => {
                        warn!("tx not included in next blocks, retrying with higher fees");
                        bump_fees = true;
                        continue;
                    }
                }
            };

            ProposerMetrics::proposed_batches(false);

            let err_str = err.to_string();
            let err = if let Ok(reason) = RevertReason::try_from(err_str.as_str()) {
                match reason {
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
                        PreconfRouterErrors::NotPreconferOrFallback(_) => {
                            warn!("failed proposing before change in operator, stop retrying");
                            break;
                        }
                        PreconfRouterErrors::InvalidLastBlockId(invalid) => {
                            let actual = invalid._actual;
                            let expected = invalid._expected;

                            if actual > expected {
                                // this batch was most likely already proposed, skip
                                warn!(%actual, %expected, "invalid expected block id, was this batch already proposed?");
                                break;
                            } else {
                                format!(
                                    "invalid last block id: actual={}, expected={}",
                                    actual, expected
                                )
                            }
                        }

                        _ => {
                            format!("unhandled PreconfRouter revert: {err:?}")
                        }
                    },

                    RevertReason::PreconfWhitelist(err) => {
                        format!("unhandled PreconfWhitelist revert: {err:?}")
                    }
                }
            } else if let Some(err) = FailReason::try_extract(&err_str) {
                match err {
                    FailReason::UnderpricedTipCap { sent, queued } => {
                        warn!(sent, queued, "tip cap underpriced, retrying with higher fees");
                        tip_cap = Some(2 * queued);
                        continue;
                    }

                    FailReason::UnderpricedBlobFeeCap { sent, queued } => {
                        warn!(sent, queued, "blob fee cap underpriced, retrying with higher fees");
                        blob_fee_cap = Some(2 * queued);
                        continue;
                    }

                    FailReason::UnderpricedGasFeeCap { sent, queued } => {
                        warn!(sent, queued, "gas fee cap underpriced, retrying with higher fees");
                        gas_fee_cap = Some(2 * queued);
                        continue;
                    }
                }
            } else {
                err_str
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

        LivePending::remove_pending();
        ProposerMetrics::proposed_batches(true);
        ProposerMetrics::batch_size_blocks(block_count as u64);

        set_propose_ok();

        if retries > 0 {
            let msg = format!("resolved propose batch, bns={start_bn}-{end_bn}");
            alert_discord(&msg);
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn propose_one_batch(
        &self,
        input: Bytes,
        sidecar: Option<BlobTransactionSidecar>,
        bump_fees: bool,
        tip_cap: Option<u128>,
        blob_fee_cap: Option<u128>,
        gas_fee_cap: Option<u128>,
        last_used_nonce: &mut u64,
    ) -> eyre::Result<()> {
        let nonce = self.client.get_nonce().await?;

        if nonce > *last_used_nonce {
            warn!("nonce is higher than override, most likely tx got already confirmed. Returning");
            return Ok(());
        }

        *last_used_nonce = nonce;

        let (tx, tx_hash, tx_type) = if let Some(sidecar) = sidecar {
            debug!(nonce, blobs = sidecar.blobs.len(), "building blob tx");
            let tx_blob = self
                .client
                .build_eip4844(
                    input,
                    nonce,
                    sidecar,
                    bump_fees,
                    tip_cap,
                    blob_fee_cap,
                    gas_fee_cap,
                    self.config.min_priority_fee,
                )
                .await?;
            if !self.config.use_cell_proof {
                // Send as normal blob tx
                (tx_blob.encoded_2718(), *tx_blob.tx_hash(), tx_blob.tx_type())
            } else {
                // Send as EIP-7594 tx with cell proof
                // https://github.com/alloy-rs/examples/blob/614f81e27ea2ac118fc73abdc9793917886479a6/examples/transactions/examples/send_eip7594_transaction.rs
                let tx_7594 = tx_blob
                    .try_into_pooled()?
                    .try_map_eip4844(|tx| {
                        tx.try_map_sidecar(|sidecar| {
                            sidecar.try_into_7594(EnvKzgSettings::Default.get())
                        })
                    })
                    .inspect_err(|e| error!(%e, "failed to convert sidecar to 7594"))?;
                (tx_7594.encoded_2718(), *tx_7594.tx_hash(), tx_7594.tx_type())
            }
        } else {
            let tx_calldata = self
                .client
                .build_eip1559(
                    input,
                    nonce,
                    bump_fees,
                    tip_cap,
                    gas_fee_cap,
                    self.config.min_priority_fee,
                )
                .await?;
            (tx_calldata.encoded_2718(), *tx_calldata.tx_hash(), tx_calldata.tx_type())
        };

        info!(nonce, bump_fees, "type" = %tx_type, %tx_hash, "sending blocks proposal tx");

        let start = Instant::now();

        let start_block = self.client.get_last_block_number().await?;

        let send_mempool_at = if let Some(url) = self.config.builder_url.as_ref() {
            let tx_encoded = format!("0x{}", hex::encode(&tx));
            let mut all_success = true;
            for slot_offset in 0..=self.lookahead.handover_window_slots() {
                let target_block = start_block + 1 + slot_offset;
                if let Err(e) = send_bundle_request(url, &tx_encoded, target_block).await {
                    warn!(%e, %url, %target_block, "failed to send bundle to builder");
                    all_success = false;
                    break;
                }
            }
            if all_success {
                Instant::now() +
                    Duration::from_secs(12)
                        .saturating_mul(self.config.builder_wait_receipt_blocks as u32)
            } else {
                Instant::now()
            }
        } else {
            Instant::now()
        };

        let mut sent_memmpool = false;
        let tx_receipt = loop {
            if !sent_memmpool && Instant::now() >= send_mempool_at {
                if self.config.builder_url.is_some() {
                    warn!("batch did not land with builder, sending to mempool");
                    ProposerMetrics::builder_fallback();
                }

                sent_memmpool = true;
                let hash = match &self.tx_send_provider {
                    None => self.client.send_raw_tx(&tx).await?,
                    Some(provider) => *provider.send_raw_transaction(&tx).await?.tx_hash(),
                };
                assert_eq!(tx_hash, hash);
            }
            if let Some(receipt) = self.client.get_tx_receipt(tx_hash).await? {
                break receipt;
            }
            let bn = self.client.get_last_block_number().await?;
            if bn > start_block + self.config.builder_wait_receipt_blocks + 2 {
                bail!(ProposerError::TxNotIncludedInNextBlocks);
            }
            tokio::time::sleep(Duration::from_secs(3)).await;
            warn!(sent_bn = start_block, l1_bn = bn, %tx_hash, "waiting for receipt")
        };

        ProposerMetrics::proposal_latency(start.elapsed());

        let tx_hash = tx_receipt.transaction_hash;
        let block_number = tx_receipt.block_number.unwrap_or_default();

        if tx_receipt.status() {
            if !sent_memmpool {
                ProposerMetrics::builder_batch();
            }

            info!(
                %tx_hash,
                l1_bn = block_number,
                "proposed batch"
            );

            let client = self.client.clone();
            spawn(async move {
                if let Err(err) = client.reorg_check(tx_hash).await {
                    ProposerMetrics::proposal_reorg();
                    let msg = format!("reorg check failed: {err}");
                    error!("{msg}");
                    panic!("{}", msg);
                };
            });

            tokio::time::sleep(Duration::from_secs(1)).await;

            Ok(())
        } else {
            *last_used_nonce = u64::MAX; // retry with new nonce
            let receipt_json = serde_json::to_string(&tx_receipt)?;
            bail!("failed to propose block: l1_bn={block_number}, tx_hash={tx_hash}, receipt={receipt_json}");
        }
    }

    // TODO: use gas cost instead of size
    fn should_use_blobs(&self, _compressed_size: usize) -> bool {
        // const CALLDATA_SIZE: usize = 50_000; // max calldata with some buffer
        // compressed_size > CALLDATA_SIZE
        // avoid address_already_in_use error
        true
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
            .get_block_by_number(bn.into())
            .full()
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
    batch_target_size: usize,
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
            .filter(|tx| tx.from() != GOLDEN_TOUCH_ADDRESS)
            .map(|tx| {
                let from = tx.from();
                Order::new_with_sender(tx.inner.into_inner(), from)
            })
            .collect::<Vec<_>>();

        let mut new_tx_list = cur_params.all_tx_list.clone();
        new_tx_list.extend(txs.clone());

        let compressed = encode_and_compress_orders(new_tx_list.clone(), false);
        if compressed.len() > batch_target_size ||
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
