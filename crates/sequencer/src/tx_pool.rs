use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use alloy_consensus::Transaction;
use alloy_primitives::Address;
use lru::LruCache;
use pc_common::sequencer::Order;
use tracing::debug;

use crate::sorting::ActiveOrders;

pub struct TxPool {
    // sender -> tx list
    txs: HashMap<Address, TxList>,
    // address -> mined_nonce (alternative would be calling transaction count on the simulator)
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
    pub fn put(&mut self, tx: Arc<Order>, source: &str) {
        let hash = *tx.tx_hash();
        let sender = *tx.sender();
        let nonce = tx.nonce();

        if let Some(db_nonce) = self.nonces.get(&sender) {
            if nonce <= *db_nonce {
                return;
            }
        }

        self.txs.entry(*tx.sender()).or_insert(TxList::new(*tx.sender())).put(tx);

        debug!(source, active = self.txs.len(), %hash, %sender, "new_tx");
    }

    /// Clear all mined nonces for each address
    pub fn clear_mined(&mut self, txs: impl Iterator<Item = (Address, u64)>) {
        for (sender, mined_nonce) in txs {
            self.nonces.push(sender, mined_nonce);
            if let Some(tx_list) = self.txs.get_mut(&sender) {
                if tx_list.forward(mined_nonce) {
                    self.txs.remove(&sender);
                }
            }
        }
    }

    pub fn active_orders(&self) -> Option<ActiveOrders> {
        (!self.txs.is_empty()).then_some(ActiveOrders::new(&self.txs))
    }
}

// nonce -> txs
#[derive(Debug, Clone)]
pub struct TxList {
    sender: Address,
    txs: BTreeMap<u64, Arc<Order>>,
}

impl TxList {
    pub fn new(sender: Address) -> Self {
        Self { sender, txs: BTreeMap::new() }
    }

    pub fn put(&mut self, tx: Arc<Order>) {
        assert_eq!(self.sender, *tx.sender());
        self.txs.insert(tx.nonce(), tx);
    }

    /// Removes all transactions with nonce lower or equal than the provided threshold.
    /// Returns true if list becomes empty after removal.
    pub fn forward(&mut self, mined_nonce: u64) -> bool {
        // TODO: fix this since it's sorted
        self.txs.retain(|nonce, _| *nonce > mined_nonce);
        self.txs.is_empty()
    }

    pub fn first_ready(&mut self) -> Option<Arc<Order>> {
        self.txs.pop_first().map(|(_, tx)| tx)
    }

    pub fn weight(&self) -> u128 {
        self.txs
            .first_key_value()
            .map(|(_, tx)| tx.priority_fee_or_price())
            .expect("txlist should be called when empty")
    }
}
