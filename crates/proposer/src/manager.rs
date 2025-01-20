use std::{collections::BTreeMap, sync::Arc, time::Duration};

use alloy_consensus::BlobTransactionSidecar;
use alloy_primitives::Bytes;
use alloy_rpc_types::Block;
use eyre::bail;
use pc_common::{
    config::ProposerConfig,
    proposer::{NewSealedBlock, ProposerContext},
    taiko::pacaya::{propose_batch_blobs, propose_batch_calldata},
    utils::alert_discord,
};
use tokio::sync::mpsc::UnboundedReceiver;
use tracing::{error, info};

use super::L1Client;

pub struct ProposerManager {
    config: ProposerConfig,
    context: ProposerContext,
    client: L1Client,
    new_blocks_rx: UnboundedReceiver<NewSealedBlock>,
}

impl ProposerManager {
    pub fn new(
        config: ProposerConfig,
        context: ProposerContext,
        client: L1Client,
        new_blocks_rx: UnboundedReceiver<NewSealedBlock>,
    ) -> Self {
        Self { config, context, client, new_blocks_rx }
    }

    pub async fn run(mut self) {
        // todo: this should be considering the time of preconfer switch, and how old is the oldest
        // anchor block id NOTE: this probably conflicts with the reorg check so cant be too
        // low
        let mut tick = tokio::time::interval(self.config.propose_frequency);

        let mut to_propose: BTreeMap<u64, Vec<NewSealedBlock>> = BTreeMap::new();

        loop {
            tokio::select! {
                Some(block) = self.new_blocks_rx.recv() => {
                    to_propose.entry(block.anchor_block_id,).or_default().push(block);
                }

                _ = tick.tick() => {
                    if let Some((anchor_block_id, blocks)) = to_propose.pop_first() {
                        info!(anchor_block_id, n_blocks = blocks.len(), "proposing batch");
                        let blocks = blocks.into_iter().map(|b| b.block).collect();
                        self.propose_batch_with_retry(anchor_block_id, blocks).await;
                    }
                }
            }
        }
    }

    async fn propose_batch_with_retry(&self, anchor_block_id: u64, full_blocks: Vec<Arc<Block>>) {
        assert!(!full_blocks.is_empty(), "no blocks to propose");

        if self.config.dry_run {
            // todo: log something
            return;
        }

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
            error!(%err, "failed to propose block, retrying in 12 secs");

            tokio::time::sleep(Duration::from_secs(12)).await;
            retries += 1;

            if retries == 1 {
                let msg = format!("failed to propose blocks={:?}, err={}", hashes, err);
                alert_discord(&msg);
            }

            if retries == MAX_RETRIES {
                let msg =
                    format!("max retries reached to propose blocks={:?}, err={}", hashes, err);
                panic!("{}", msg);
            }
        }

        if retries > 0 {
            let msg = format!("resolved propose blocks={:?}", hashes);
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

// /// If we restart and some blocks were not proposed, fetch from preconf tip and re-send proposals
// #[allow(clippy::comparison_chain)]
// #[tracing::instrument(skip_all, name = "resync", fields(force_reorgs = force_reorgs))]
// pub async fn resync_blocks(
//     l1_url: Url,
//     taiko_config: TaikoConfig,
//     proposer: Arc<Propose>,
//     includer: Arc<L1Includer>,
//     force_reorgs: bool,
// ) -> eyre::Result<()> {
//     let l1_provider = ProviderBuilder::new().on_http(l1_url);
//     let chain_provider = ProviderBuilder::new().on_http(taiko_config.rpc_url);
//     let preconf_provider = ProviderBuilder::new().on_http(taiko_config.preconf_url);

//     let mut chain_head = chain_provider.get_block_number().await?;
//     let mut preconf_head = preconf_provider.get_block_number().await?;

//     info!(chain_head, preconf_head, "resyncing preconf blocks");

//     if chain_head > preconf_head {
//         // TODO: this can happen in a edge case where we a block has just been proposed and
//         // simulator hasn't synced yet
//         bail!("simulator is behind chain, this should not happen")
//     } else if chain_head < preconf_head {
//         while chain_head < preconf_head {
//             let mut requests = vec![];

//             let l1_head = l1_provider
//                 .get_block_by_number(BlockNumberOrTag::Latest, BlockTransactionsKind::Hashes)
//                 .await?
//                 .ok_or_eyre("failed l1 fetch")?;

//             let mut to_verify = HashMap::new();
//             let mut has_reorged = false;

//             // sync max 10 blocks at a time
//             let max_preconf_bn = preconf_head.min(chain_head + 1 + 10);
//             for bn in (chain_head + 1)..=max_preconf_bn {
//                 let block = preconf_provider
//                     .get_block_by_number(bn.into(), BlockTransactionsKind::Full)
//                     .await?
//                     .ok_or_eyre("failed to get block")?;
//                 let block = Arc::new(block);
//                 to_verify.insert(bn, block.clone());

//                 // unwraps are ok because hydrate = true and there's at least one tx
//                 let anchor_tx = block.transactions.as_transactions().unwrap().first().unwrap();

//                 assert_eq!(
//                     anchor_tx.from, GOLDEN_TOUCH_ADDRESS,
//                     "expected first tx to be from anchor"
//                 );

//                 // FIXME
//                 // get the anchor data
//                 let anchor_data =
//                     ontake::l2::TaikoL2::anchorV2Call::abi_decode(&anchor_tx.input(), true)?;
//                 let anchor_block_id = anchor_data._anchorBlockId;
//                 let anchor_timestamp = block.header.timestamp;

//                 let reorg_by_number = l1_head.header.number.saturating_sub(anchor_block_id)
//                     >= taiko_config.chain.max_anchor_height_offset;
//                 let reorg_by_timestamp =
// l1_head.header.timestamp.saturating_sub(anchor_timestamp)                     >=
// taiko_config.chain.max_anchor_timestamp_offset;

//                 if reorg_by_number || reorg_by_timestamp {
//                     // this can happen if we waited too long before restarting the proposer, a
//                     // re-org is inevitable
//                     if force_reorgs {
//                         let request = NewSealedBlock {
//                             block,
//                             anchor_block_id: l1_head.header.number - 1,
//                             anchor_timestamp: l1_head.header.timestamp - 12,
//                         };
//                         requests.push(request);
//                         has_reorged = true;
//                     } else {
//                         panic!("unrecoverable re-org (number), check manually");
//                     }
//                 } else {
//                     // propose same block
//                     let request = NewSealedBlock { block, anchor_block_id, anchor_timestamp };
//                     requests.push(request);
//                 }
//             }

//             if has_reorged {
//                 let msg = format!("re-orged blocks {}-{}", chain_head + 1, max_preconf_bn);
//                 alert_discord(&msg);
//             }

//             propose_blocks(requests, &proposer, &includer, false).await?;

//             if !has_reorged {
//                 for bn in (chain_head + 1)..=max_preconf_bn {
//                     let preconf_block = to_verify.remove(&bn).unwrap();
//                     let new_block = chain_provider
//                         .get_block_by_number(bn.into(), BlockTransactionsKind::Hashes)
//                         .await?
//                         .ok_or_eyre("failed to get chain block")?;

//                     verify_and_log_block(&preconf_block.header, &new_block.header, true);
//                 }
//             }

//             chain_head = chain_provider.get_block_number().await?;
//             preconf_head = preconf_provider.get_block_number().await?;
//         }
//     }

//     Ok(())
// }
