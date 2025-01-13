use std::sync::Arc;

use alloy_consensus::TxEnvelope;
use alloy_rpc_types::{Block, BlockTransactions};
use tracing::{debug, error};

use crate::taiko::GOLDEN_TOUCH_ADDRESS;

#[derive(Debug, Clone)]
pub struct ProposerRequest {
    pub block: Arc<Block>,
    pub anchor_block_id: u64,
    pub anchor_timestamp: u64,
}

#[derive(Debug, Clone)]
pub struct ProposeParams {
    pub anchor_block_id: u64,
    pub anchor_timestamp: u64,
    pub tx_list: Vec<TxEnvelope>,
}

impl From<ProposerRequest> for ProposeParams {
    fn from(req: ProposerRequest) -> Self {
        let tx_list = extract_transactions(&req.block);

        assert_eq!(
            tx_list.len(),
            req.block.transactions.len().saturating_sub(1),
            "received block without anchor transaction"
        );

        Self {
            anchor_block_id: req.anchor_block_id,
            anchor_timestamp: req.anchor_timestamp,
            tx_list,
        }
    }
}

// FIXME
fn extract_transactions(block: &Block) -> Vec<TxEnvelope> {
    match block.transactions.clone() {
        BlockTransactions::Full(txs) => txs
            .into_iter()
            .filter_map(|tx| {
                let hash = *tx.inner.tx_hash();
                let index = tx.transaction_index.expect("missing tx index");

                // exclude anchor from tx list
                if tx.from == GOLDEN_TOUCH_ADDRESS {
                    debug!(index, %hash, "skipping anchor tx");
                    None
                } else {
                    match TxEnvelope::try_from(tx).ok() {
                        None => {
                            error!(%hash, "failed to decode tx");
                            None
                        }
                        Some(envelope) => {
                            debug!(index, %hash, "adding user tx");
                            Some(envelope)
                        }
                    }
                }
            })
            .collect::<Vec<_>>(),
        // TODO: return warning
        BlockTransactions::Uncle => unreachable!("uncle block"),
        BlockTransactions::Hashes(_) => unreachable!("hashes block"),
    }
}
