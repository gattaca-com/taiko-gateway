use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use alloy_consensus::Transaction;
use alloy_primitives::Address;
use pc_common::sequencer::Order;

pub struct TxPool {
    // sender -> tx list
    txs: HashMap<Address, TxList>,
}

// TODO: improve this
// currently not doing much since we need to make async calls to simulator for most checks
impl TxPool {
    pub fn new() -> Self {
        Self { txs: HashMap::new() }
    }

    /// Inserts a tx in the pool, overwriting any existing tx with the same nonce
    pub fn put(&mut self, tx: Arc<Order>) {
        self.txs.entry(*tx.sender()).or_default().put(tx);
    }

    /// Clear all invalid nonces for each address
    pub fn clear_mined(&mut self, txs: impl Iterator<Item = (Address, u64)>) {
        for (sender, nonce) in txs {
            if let Some(txs) = self.txs.get_mut(&sender) {
                if txs.forward(nonce) {
                    self.txs.remove(&sender);
                }
            }
        }
    }

    // TODO: this is not efficient, we should get some top of block payments at least
    pub fn next_sequence(&mut self) -> Option<Arc<Order>> {
        self.txs.values_mut().next().and_then(|txs| txs.first_ready())
    }
}

// nonce -> txs
#[derive(Default)]
struct TxList(BTreeMap<u64, Arc<Order>>);

impl TxList {
    pub fn put(&mut self, tx: Arc<Order>) {
        self.0.insert(tx.nonce(), tx);
    }

    /// Removes all transactions with nonce lower or equal than the provided threshold.
    /// Returns true if list becomes empty after removal.
    pub fn forward(&mut self, mined_nonce: u64) -> bool {
        if self.0.first_key_value().is_some_and(|(nonce, _)| *nonce > mined_nonce) {
            return false;
        }

        while let Some((nonce, _)) = self.0.pop_first() {
            if nonce == mined_nonce {
                break;
            }
        }

        self.0.is_empty()
    }

    pub fn first_ready(&mut self) -> Option<Arc<Order>> {
        self.0.pop_first().map(|(_, tx)| tx)
    }
}
