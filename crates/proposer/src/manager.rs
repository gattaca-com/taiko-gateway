#![allow(clippy::comparison_chain)]

use std::{
    collections::BTreeMap, sync::Arc, time::Duration
};

use alloy_consensus::BlobTransactionSidecar;
use alloy_primitives::Bytes;
use alloy_rpc_types::Block;
use eyre::bail;
use pc_common::{
    config::{ProposerConfig, TaikoConfig},
    proposer::{is_resyncing, set_propose_delayed, set_propose_ok, ProposerContext, ProposerEvent},
    taiko::pacaya::{propose_batch_blobs, propose_batch_calldata},
    utils::alert_discord,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{error, info, warn};

use crate::manager_resync::RESYNC_MAX_RETRIES;

use super::L1Client;

pub(crate) type AlloyProvider = alloy_provider::RootProvider;

pub struct ProposerManager {
    config: ProposerConfig,
    context: ProposerContext,
    pub(crate) client: L1Client,
    events_rx: UnboundedReceiver<ProposerEvent>,

    pub(crate) l2_provider: AlloyProvider,
    pub(crate) preconf_provider: AlloyProvider,
    pub(crate) taiko_config: TaikoConfig,
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
        Self { config, context, client, events_rx, l2_provider, preconf_provider, taiko_config }
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
                    info!("received event: {}", event);
                    match event {
                        ProposerEvent::SealedBlock { block, anchor_block_id } => {
                            to_propose.entry(anchor_block_id).or_default().push(block);
                        }
                        ProposerEvent::NeedsResync => {
                            warn!("received resync request from sequencer");
                            // Clear pending proposals as they may be invalid
                            to_propose.clear();
                            
                            // Trigger resync
                            if let Err(err) = self.resync_with_retries(RESYNC_MAX_RETRIES).await {
                                error!(%err, "failed to resync");
                                alert_discord(&format!("failed to resync: {}, the gateway will be stopped!", err));
                                panic!("failed to resync: {}", err);
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

    pub(crate) async fn propose_batch_with_retry(&self, anchor_block_id: u64, full_blocks: Vec<Arc<Block>>) {
        assert!(!full_blocks.is_empty(), "no blocks to propose");

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