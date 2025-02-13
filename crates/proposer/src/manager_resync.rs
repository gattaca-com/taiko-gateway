use std::{collections::{BTreeMap, HashMap}, sync::Arc};

use alloy_consensus::Transaction;
use alloy_provider::Provider;
use alloy_rpc_types::{Block, BlockTransactionsKind};
use alloy_sol_types::SolCall;
use eyre::{bail, ensure, OptionExt, eyre};
use pc_common::{proposer::{set_resync_complete, set_resyncing}, taiko::{pacaya::l2::TaikoL2, GOLDEN_TOUCH_ADDRESS}, utils::{alert_discord, verify_and_log_block}};
use tracing::{info, warn};

use crate::manager::{AlloyProvider, ProposerManager};



impl ProposerManager {
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
