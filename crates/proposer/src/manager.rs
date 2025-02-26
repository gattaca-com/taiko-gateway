#![allow(clippy::comparison_chain)]

use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
    time::Duration,
};

use alloy_consensus::{BlobTransactionSidecar, Transaction};
use alloy_primitives::Bytes;
use alloy_provider::Provider;
use alloy_rpc_types::{Block, BlockTransactionsKind};
use alloy_sol_types::SolCall;
use eyre::{bail, ensure, eyre, OptionExt};
use pc_common::{
    config::{ProposerConfig, TaikoConfig},
    proposer::{set_propose_delayed, set_propose_ok, ProposalRequest, ProposerContext},
    taiko::{
        pacaya::{
            encode_and_compress_tx_list, l2::TaikoL2, propose_batch_blobs, propose_batch_calldata,
            BlockParams,
        },
        GOLDEN_TOUCH_ADDRESS,
    },
    utils::{alert_discord, verify_and_log_block},
};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{error, info, warn};

use super::L1Client;

type AlloyProvider = alloy_provider::RootProvider;

pub struct ProposerManager {
    config: ProposerConfig,
    context: ProposerContext,
    client: L1Client,
    new_blocks_rx: UnboundedReceiver<ProposalRequest>,
}

impl ProposerManager {
    pub fn new(
        config: ProposerConfig,
        context: ProposerContext,
        client: L1Client,
        new_blocks_rx: UnboundedReceiver<ProposalRequest>,
    ) -> Self {
        Self { config, context, client, new_blocks_rx }
    }

    #[tracing::instrument(skip_all, name = "proposer")]
    pub async fn run(mut self) {
        info!(proposer_address = %self.client.address(), "starting l1 proposer");

        while let Some(request) = self.new_blocks_rx.recv().await {
            info!(
                n_blocks = request.block_params.len(),
                all_txs = request.all_tx_list.len(),
                "proposing batch for blocks {}-{}",
                request.start_block_num,
                request.end_block_num
            );
            self.propose_batch_with_retry(request).await;
        }
    }

    #[tracing::instrument(skip_all, name = "resync")]
    pub async fn needs_resync(
        &self,
        l2_provider: &AlloyProvider,
        preconf_provider: &AlloyProvider,
    ) -> eyre::Result<bool> {
        let chain_head = l2_provider
            .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest, false.into())
            .await?
            .ok_or_eyre("missing last block")?;

        let preconf_head = preconf_provider
            .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest, false.into())
            .await?
            .ok_or_eyre("missing last block")?;

        info!(chain_head = %chain_head.header.number, preconf_head = %preconf_head.header.number, "checking resync");

        if chain_head == preconf_head {
            ensure!(
                chain_head.header.hash == preconf_head.header.hash,
                "chain and preconf mismatch for block number {}",
                chain_head.header.number
            );
            Ok(false)
        } else if chain_head.header.number > preconf_head.header.number {
            bail!("chain is ahead of preconf, simulator is out of sync (this should not happen)");
        } else {
            Ok(true)
        }
    }

    #[tracing::instrument(skip_all, name = "resync")]
    pub async fn resync(
        &self,
        l2_provider: &AlloyProvider,
        preconf_provider: &AlloyProvider,
        taiko_config: &TaikoConfig,
    ) -> eyre::Result<()> {
        let mut l1_head = self.client.provider().get_block_number().await?;
        let mut chain_head = l2_provider
            .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest, false.into())
            .await?
            .ok_or_eyre("missing last block")?;

        let mut preconf_head = preconf_provider
            .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest, false.into())
            .await?
            .ok_or_eyre("missing last block")?;

        let mut to_verify = HashMap::new();
        let mut has_reorged = false;

        while chain_head.header.number < preconf_head.header.number {
            // TODO: double check this
            // NOTE: if a block with a given anchor block id is not proposed in the correct batch,
            // it will be necessarily reorged
            let blocks = fetch_n_blocks(
                preconf_provider,
                chain_head.header.number + 1,
                preconf_head.header.number,
            )
            .await?;

            let mut to_propose: BTreeMap<u64, Vec<Arc<Block>>> = BTreeMap::new();

            for block in blocks {
                let bn = block.header.number;
                to_verify.insert(bn, block.clone());

                let anchor_tx = block
                    .transactions
                    .as_transactions()
                    .and_then(|txs| txs.first())
                    .ok_or_eyre(format!("missing anchor tx in block {bn}"))?;

                ensure!(
                    anchor_tx.from == GOLDEN_TOUCH_ADDRESS,
                    "expected anchor tx to be from golden touch"
                );

                let anchor_data = TaikoL2::anchorV3Call::abi_decode(anchor_tx.input(), true)?;
                let anchor_block_id = anchor_data._anchorBlockId;
                to_propose.entry(anchor_block_id).or_default().push(block);
            }

            for (anchor_block_id, blocks) in to_propose {
                let reorg_by_number = l1_head.saturating_sub(anchor_block_id) >=
                    taiko_config.params.max_anchor_height_offset;
                let reorg_by_timestamp = false; // TODO: implement

                if reorg_by_number || reorg_by_timestamp {
                    has_reorged = true;

                    let msg = format!(
                        "re-orged blocks {}-{}",
                        blocks[0].header.number, preconf_head.header.number
                    );
                    warn!("{msg}");
                    alert_discord(&msg);

                    // this can happen if we waited too long before restarting the proposer, a
                    // re-org is inevitable
                    let request = request_from_blocks(l1_head - 1, blocks);
                    self.propose_batch_with_retry(request).await;
                } else {
                    // propose same batch
                    let request = request_from_blocks(anchor_block_id, blocks);
                    self.propose_batch_with_retry(request).await;
                }
            }

            l1_head = self.client.provider().get_block_number().await?;

            chain_head = l2_provider
                .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest, false.into())
                .await?
                .ok_or_eyre("missing last block")?;

            preconf_head = preconf_provider
                .get_block_by_number(alloy_eips::BlockNumberOrTag::Latest, false.into())
                .await?
                .ok_or_eyre("missing last block")?;
        }

        if !has_reorged {
            for (bn, block) in to_verify {
                let new_block = l2_provider
                    .get_block_by_number(bn.into(), BlockTransactionsKind::Hashes)
                    .await?
                    .ok_or(eyre!("missing proposed block {bn}"))?;

                verify_and_log_block(&block.header, &new_block.header, true);
            }
        }

        Ok(())
    }

    async fn propose_batch_with_retry(&self, request: ProposalRequest) {
        let start_bn = request.start_block_num;
        let end_bn = request.end_block_num;

        debug_assert!(!request.block_params.is_empty(), "no blocks to propose");

        if self.config.dry_run {
            warn!("dry run, skipping proposal");
            return;
        }

        let parent_meta_hash = self.client.last_meta_hash().await;

        let compressed = encode_and_compress_tx_list(request.all_tx_list.clone()).into(); // FIXME
        let should_use_blobs = self.should_use_blobs(&compressed);
        let (input, maybe_sidecar) = if should_use_blobs {
            let (input, sidecar) = propose_batch_blobs(request, parent_meta_hash, &self.context);
            (input, Some(sidecar))
        } else {
            let input = propose_batch_calldata(request, parent_meta_hash, &self.context);
            (input, None)
        };

        const MAX_RETRIES: usize = 3;
        let mut retries = 0;

        while let Err(err) = self.propose_one_batch(input.clone(), maybe_sidecar.clone()).await {
            set_propose_delayed();
            retries += 1;

            if retries == 1 {
                let msg = format!(
                    "failed to propose batch, retrying in 12 secs. bns={start_bn}-{end_bn}, err={err:?}"
                );
                error!("{msg}");
                alert_discord(&msg);
            }

            if retries == MAX_RETRIES {
                let msg = format!(
                    "max retries reached to propose batch. bns={start_bn}-{end_bn}, err={err}"
                );
                panic!("{}", msg);
            }

            tokio::time::sleep(Duration::from_secs(12)).await;
        }

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
            self.client.build_eip4844(input, sidecar).await?
        } else {
            self.client.build_eip1559(input).await?
        };

        info!(tx_hash = %tx.tx_hash(), "sending blocks proposal tx");
        let tx_receipt = self.client.send_tx(tx).await?;

        let tx_hash = tx_receipt.transaction_hash;
        let block_number = tx_receipt.block_number.unwrap_or_default();

        if tx_receipt.status() {
            info!(
                %tx_hash,
                l1_bn = block_number,
                "proposed batch"
            );

            self.client.reorg_check(tx_hash).await?;

            Ok(())
        } else {
            let receipt_json = serde_json::to_string(&tx_receipt)?;
            bail!("failed to propose block: l1_bn={block_number}, tx_hash={tx_hash}, receipt={receipt_json}");
        }
    }

    fn should_use_blobs(&self, compressed: &Bytes) -> bool {
        const CALLDATA_SIZE: usize = 100_000; // max calldata with some buffer
        const _BLOBS_SAFE_SIZE: usize = 125_000; // 131072 with some buffer
        const _MAX_BLOBS_SIZE: usize = 3 * _BLOBS_SAFE_SIZE; // use at most 3 blobs

        compressed.len() > CALLDATA_SIZE
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
            .ok_or_eyre("missing block")?;
        blocks.push(block.into());
    }

    Ok(blocks)
}

fn request_from_blocks(anchor_block_id: u64, full_blocks: Vec<Arc<Block>>) -> ProposalRequest {
    let start_block_num = full_blocks[0].header.number;
    let end_block_num = full_blocks[full_blocks.len() - 1].header.number;

    let mut blocks = Vec::with_capacity(full_blocks.len());
    let mut tx_list = Vec::new();
    let mut last_timestamp = 0;

    for block in full_blocks {
        let block = Arc::unwrap_or_clone(block);

        blocks.push(BlockParams {
            numTransactions: (block.transactions.len() - 1) as u16, // remove anchor
            timeShift: 0,
            signalSlots: vec![], // TODO
        });

        let txs = block
            .transactions
            .into_transactions()
            .filter(|tx| tx.from != GOLDEN_TOUCH_ADDRESS)
            .map(|tx| tx.inner);

        tx_list.extend(txs);
        last_timestamp = block.header.timestamp;
    }

    ProposalRequest {
        anchor_block_id,
        start_block_num,
        end_block_num,
        block_params: blocks,
        all_tx_list: tx_list,
        last_timestamp,
    }
}
