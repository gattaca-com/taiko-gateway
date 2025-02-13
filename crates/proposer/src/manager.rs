#![allow(clippy::comparison_chain)]

use std::{
    collections::{BTreeMap, HashMap}, sync::{atomic::AtomicBool, Arc, Mutex}, time::Duration
};

use alloy_consensus::{BlobTransactionSidecar, Transaction};
use alloy_primitives::Bytes;
use alloy_provider::Provider;
use alloy_rpc_types::{Block, BlockTransactionsKind};
use alloy_sol_types::SolCall;
use eyre::{bail, ensure, eyre, OptionExt};
use pc_common::{
    config::{ProposerConfig, TaikoConfig},
    proposer::{is_resyncing, set_propose_delayed, set_propose_ok, set_resync_complete, set_resyncing, ProposerContext, ProposerEvent},
    taiko::{
        pacaya::{l2::TaikoL2, propose_batch_blobs, propose_batch_calldata},
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
    events_rx: UnboundedReceiver<ProposerEvent>,

    l2_provider: AlloyProvider,
    preconf_provider: AlloyProvider,
    taiko_config: TaikoConfig,

    // for testing
    dry_run: Mutex<AtomicBool>,
}

impl ProposerManager {
    pub fn new(
        config: ProposerConfig,
        context: ProposerContext,
        client: L1Client,
        events_rx: UnboundedReceiver<ProposerEvent>,
        l2_provider: AlloyProvider,
        preconf_provider: AlloyProvider,
        taiko_config: TaikoConfig,
    ) -> Self {
        Self { config, context, client, events_rx, l2_provider, preconf_provider, taiko_config, dry_run: Mutex::new(AtomicBool::new(true)) }
    }

    #[tracing::instrument(skip_all, name = "proposer")]
    pub async fn run(mut self) {
        info!(proposer_address = %self.client.address(), "starting l1 proposer");

        // todo: this should be considering the time of preconfer switch, and how old is the oldest
        // anchor block id

        // NOTE: this probably conflicts with the reorg check so cant be too low
        let mut tick = tokio::time::interval(self.config.propose_frequency);

        // batches to propose sorted by anchor block id
        let mut to_propose: BTreeMap<u64, Vec<Arc<Block>>> = BTreeMap::new();

        loop {
            tokio::select! {
                Some(event) = self.events_rx.recv() => {
                    info!("received event: {:?}", event);
                    match event {
                        ProposerEvent::SealedBlock { block, anchor_block_id } => {
                            to_propose.entry(anchor_block_id).or_default().push(block);
                        }
                        ProposerEvent::NeedsResync => {
                            warn!("received resync request from sequencer");
                            // Clear pending proposals as they may be invalid
                            to_propose.clear();
                            
                            // Trigger resync
                            if let Err(err) = self.resync().await {
                                error!(%err, "failed to resync");
                            }
                        }
                    }
                }

                _ = tick.tick() => {
                    if !is_resyncing() {
                        if let Some((anchor_block_id, blocks)) = to_propose.pop_first() {
                            info!(anchor_block_id, n_blocks = blocks.len(), "proposing batch");
                            self.propose_batch_with_retry(anchor_block_id, blocks).await;
                        }
                    }
                }
            }
        }
    }

    #[tracing::instrument(skip_all, name = "resync")]
    pub async fn resync(&self) -> eyre::Result<()> {
        // Set resyncing flag at start
        set_resyncing();

        info!("resyncing");

        // Ensure we always reset the resyncing flag when exiting this function
        let result = async {
            const MAX_RETRIES: usize = 10;
            let mut retry_count = 0;

            'resync: loop {
                let mut l1_head = self.client.provider().get_block_number().await?;
                let mut chain_head = self.l2_provider.get_block_number().await?;
                let mut preconf_head = self.preconf_provider.get_block_number().await?;

                info!(chain_head, preconf_head, "checking resync");

                if chain_head == preconf_head {
                    return Ok(());
                } else if chain_head > preconf_head {
                    bail!("chain is ahead of preconf, simulator is out of sync (this should not happen)");
                }

                let mut to_verify = HashMap::new();
                let mut has_reorged = false;

                while chain_head < preconf_head {
                    // TODO: double check this
                    // NOTE: if a block with a given anchor block id is not proposed in the correct batch,
                    // it will be necessarily reorged
                    let blocks = fetch_n_blocks(&self.preconf_provider, chain_head + 1, preconf_head).await?;

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
                            self.taiko_config.params.max_anchor_height_offset;
                        let reorg_by_timestamp = false; // TODO: implement

                        if reorg_by_number || reorg_by_timestamp {
                            has_reorged = true;

                            let msg =
                                format!("re-orged blocks {}-{}", blocks[0].header.number, preconf_head);
                            warn!("{msg}");
                            alert_discord(&msg);

                            // this can happen if we waited too long before restarting the proposer, a
                            // re-org is inevitable
                            self.propose_batch_with_retry(l1_head - 1, blocks).await;
                        } else {
                            // propose same batch
                            self.propose_batch_with_retry(anchor_block_id, blocks).await;
                        }
                    }

                    l1_head = self.client.provider().get_block_number().await?;
                    chain_head = self.l2_provider.get_block_number().await?;
                    preconf_head = self.preconf_provider.get_block_number().await?;
                }

                if !has_reorged {
                    for (bn, block) in to_verify {
                        let new_block = self.l2_provider
                            .get_block_by_number(bn.into(), BlockTransactionsKind::Hashes)
                            .await?
                            .ok_or(eyre!("missing proposed block {bn}"))?;

                        if !verify_and_log_block(&block.header, &new_block.header, false) {
                            retry_count += 1;
                            if retry_count >= MAX_RETRIES {
                                bail!("block verification failed after {MAX_RETRIES} attempts for block {bn}");
                            }

                            let msg = format!("block verification failed for block {bn}, retrying resync (attempt {}/{})", 
                                retry_count, MAX_RETRIES);
                            warn!("{msg}");
                            alert_discord(&msg);
                            
                            // Resync again
                            continue 'resync;
                        }
                    }
                }

                // Resync was successful
                break;
            }
            Ok(())
        }.await;

        info!("resync complete");

        // Clear resyncing flag when done
        set_resync_complete();
        
        // Return the result
        result
    }

    async fn propose_batch_with_retry(&self, anchor_block_id: u64, full_blocks: Vec<Arc<Block>>) {
        assert!(!full_blocks.is_empty(), "no blocks to propose");

        /* 
        if self.dry_run.lock().unwrap().load(std::sync::atomic::Ordering::Relaxed) {
            warn!("dry run, skipping proposal");
            self.dry_run.lock().unwrap().store(false, std::sync::atomic::Ordering::Relaxed);
            return;
        }
        */

        //self.dry_run.lock().unwrap().store(true, std::sync::atomic::Ordering::Relaxed);

        let bns = full_blocks.iter().map(|b| b.header.number).collect::<Vec<_>>();
        let hashes = full_blocks.iter().map(|b| b.header.hash).collect::<Vec<_>>();
        let parent_meta_hash = self.client.last_meta_hash().await;

        let (input, maybe_sidecar) = if self.config.use_blobs {
            let (input, sidecar) =
                propose_batch_blobs(full_blocks, anchor_block_id, parent_meta_hash, &self.context);
            (input, Some(sidecar))
        } else {
            let input = propose_batch_calldata(
                full_blocks,
                anchor_block_id,
                parent_meta_hash,
                &self.context,
            );
            (input, None)
        };

        const MAX_RETRIES: usize = 3;
        let mut retries = 0;

        while let Err(err) = self.propose_one_batch(input.clone(), maybe_sidecar.clone()).await {
            set_propose_delayed();
            retries += 1;

            if retries == 1 {
                let msg = format!(
                    "failed to propose batch, retrying in 12 secs. hashes={hashes:?}, bns={bns:?}, err={err:?}"
                );
                error!("{msg}");
                alert_discord(&msg);
            }

            if retries == MAX_RETRIES {
                let msg =
                    format!("max retries reached to propose batch. hashes={hashes:?}, bns={bns:?}, err={err}");
                panic!("{}", msg);
            }

            tokio::time::sleep(Duration::from_secs(12)).await;
        }

        set_propose_ok();

        if retries > 0 {
            let msg = format!("resolved propose batch, hashes={hashes:?}, bns={bns:?}");
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
