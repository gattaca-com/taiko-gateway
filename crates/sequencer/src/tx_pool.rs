use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use alloy_consensus::Transaction;
use alloy_primitives::Address;
use lru::LruCache;
use pc_common::sequencer::Order;
use tracing::info;

use crate::sorting::ActiveOrders;

pub struct TxPool {
    // sender -> tx list
    txs: HashMap<Address, TxList>,
    // address -> next_nonce (alternative would be calling transaction count on the simulator)
    nonces: LruCache<Address, u64>,
}

// TODO: improve this
// currently not doing much since we need to make async calls to simulator for most checks
impl TxPool {
    pub fn new() -> Self {
        // this should be big enough for all the users in a single block
        let nonces = LruCache::new(10_000.try_into().unwrap());
        Self { txs: HashMap::new(), nonces }
    }

    /// Inserts a tx in the pool, overwriting any existing tx with the same nonce
    pub fn put(&mut self, tx: Arc<Order>) {
        let sender = *tx.sender();

        let next_nonce = self.nonces.get(&sender).map(|&nonce| nonce);

        if let Some(next_nonce) = next_nonce {
            if tx.nonce() < next_nonce {
                return;
            }
        }

        self.txs.entry(*tx.sender()).or_insert(TxList::new(*tx.sender(), next_nonce)).put(tx);
    }

    /// Clear all mined nonces for each address
    pub fn clear_mined(&mut self, txs: impl Iterator<Item = (Address, u64)>) {
        for (sender, mined_nonce) in txs {
            let next_nonce = mined_nonce + 1;
            self.nonces.push(sender, next_nonce);
            if let Some(tx_list) = self.txs.get_mut(&sender) {
                if tx_list.forward(next_nonce) {
                    self.txs.remove(&sender);
                }
            }
        }
    }

    pub fn active_orders(&self) -> Option<ActiveOrders> {
        if self.txs.iter().any(|(_, tx_list)| tx_list.has_ready_txs()) {
            return Some(ActiveOrders::new(&self.txs));
        } else {
            None
        }
    }
}

#[derive(Debug, Clone)]
pub struct TxList {
    pub next_nonce: Option<u64>,
    sender: Address,
    txs: BTreeMap<u64, Arc<Order>>,
}

impl TxList {
    pub fn new(sender: Address, next_nonce: Option<u64>) -> Self {
        Self { next_nonce, sender, txs: BTreeMap::new() }
    }

    pub fn sender(&self) -> &Address {
        &self.sender
    }

    pub fn put(&mut self, tx: Arc<Order>) {
        assert_eq!(self.sender, *tx.sender());
        self.txs.insert(tx.nonce(), tx);
    }

    /// Removes all transactions with nonce lower than the next_nonce.
    /// Returns true if list becomes empty after removal.
    pub fn forward(&mut self, next_nonce: u64) -> bool {
        self.next_nonce = Some(next_nonce);
        // TODO: fix this since it's sorted
        self.txs.retain(|nonce, _| *nonce >= next_nonce);
        self.txs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.txs.len()
    }

    /// Has either the correct tx (next nonce) or we havent simulated yet this nonce
    pub fn has_ready_txs(&self) -> bool {
        if let Some(next_nonce) = self.next_nonce {
            self.txs.first_key_value().map(|(_, tx)| tx.nonce()) == Some(next_nonce)
        } else {
            true
        }
    }

    pub fn first_ready(&mut self) -> Option<Arc<Order>> {
        // if we have the next nonce check that the first tx has the correct nonce
        if let Some(next_nonce) = self.next_nonce {
            if self.txs.first_key_value().map(|(_, tx)| tx.nonce()) == Some(next_nonce) {
                self.txs.pop_first().map(|(_, tx)| tx)
            } else {
                None
            }
        } else {
            // otherwise just return the first, it will be simulated and we'll update the next_nonce
            self.txs.pop_first().map(|(_, tx)| tx)
        }
    }

    pub fn weight(&self) -> u128 {
        self.txs
            .first_key_value()
            .map(|(_, tx)| tx.priority_fee_or_price())
            .expect("txlist shouldn't be called when empty")
    }
}
