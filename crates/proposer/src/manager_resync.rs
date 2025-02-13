use std::{collections::{BTreeMap, HashMap}, sync::Arc};

use alloy_consensus::Transaction;
use alloy_provider::Provider;
use alloy_rpc_types::{Block, BlockTransactionsKind};
use alloy_sol_types::SolCall;
use eyre::{bail, ensure, OptionExt, eyre};
use pc_common::{proposer::{set_resync_complete, set_resyncing}, taiko::{pacaya::l2::TaikoL2, GOLDEN_TOUCH_ADDRESS}, utils::{alert_discord, verify_and_log_block}};
use tracing::{info, warn};

use crate::manager::{AlloyProvider, ProposerManager};

pub const RESYNC_MAX_RETRIES: usize = 5;

impl ProposerManager {
    #[tracing::instrument(skip_all, name = "resync")]
    pub async fn resync_with_retries(&self, max_retries: usize) -> eyre::Result<()> {
        // Set resyncing flag at start
        set_resyncing();
        info!("resyncing");

        let mut retry_count = 0;
        
        loop {
            match self.resync_once().await {
                Ok(_) => break,
                Err(e) => {
                    retry_count += 1;
                    if retry_count >= max_retries {
                        return Err(e);
                    }
                    
                    let msg = format!(
                        "resync failed, retrying (attempt {}/{})", 
                        retry_count, max_retries
                    );
                    warn!("{msg}");
                    alert_discord(&msg);
                }
            }
        }

        info!("resync complete");
        set_resync_complete();

        Ok(())
    }

    async fn resync_once(&self) -> eyre::Result<()> {
        let result = async {
            let l1_head = self.client.provider().get_block_number().await?;
            let chain_head = self.l2_provider.get_block_number().await?;
            let preconf_head = self.preconf_provider.get_block_number().await?;

            info!(chain_head, preconf_head, "checking resync");

            if chain_head == preconf_head {
                return Ok(());
            } else if chain_head > preconf_head {
                bail!("chain is ahead of preconf, simulator is out of sync (this should not happen)");
            }

            let mut to_verify = HashMap::new();
            let (has_reorged, to_verify) = self.process_pending_blocks(
                chain_head,
                preconf_head,
                l1_head,
                &mut to_verify,
            ).await?;

            if !has_reorged {
                self.verify_blocks(&to_verify).await?;
            }

            Ok(())
        }.await;
        result
    }

    async fn process_pending_blocks(
        &self,
        mut chain_head: u64,
        mut preconf_head: u64,
        mut l1_head: u64,
        to_verify: &mut HashMap<u64, Arc<Block>>,
    ) -> eyre::Result<(bool, HashMap<u64, Arc<Block>>)> {
        let mut has_reorged = false;

        while chain_head < preconf_head {
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

                    let msg = format!("re-orged blocks {}-{}", blocks[0].header.number, preconf_head);
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

        Ok((has_reorged, to_verify.clone()))
    }

    async fn verify_blocks(
        &self,
        to_verify: &HashMap<u64, Arc<Block>>
    ) -> eyre::Result<()> {
        for (bn, block) in to_verify {
            let new_block = self.l2_provider
                .get_block_by_number((*bn).into(), BlockTransactionsKind::Hashes)
                .await?
                .ok_or(eyre!("missing proposed block {bn}"))?;

            if !verify_and_log_block(&block.header, &new_block.header, false) {
                bail!("block verification failed for block {bn}");
            }
        }
        Ok(())
    }
}

pub(crate) async fn fetch_n_blocks(
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
