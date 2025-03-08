use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use alloy_consensus::Transaction;
use alloy_primitives::Address;
use pc_common::sequencer::Order;
use tracing::debug;

use crate::{sorting::ActiveOrders, types::StateNonces};

pub struct TxPool {
    // sender -> tx list
    tx_lists: HashMap<Address, TxList>,
    // address -> state_nonce valid at block `parent_block` + 1 (alternative would be calling
    // transaction count on the simulator)
    nonces: HashMap<Address, u64>,
    /// The nonces are valid for this block number + 1
    parent_block: u64,
}

// TODO: improve this
// currently not doing much since we need to make async calls to simulator for most checks
impl TxPool {
    pub fn new() -> Self {
        // this should be big enough for all the users in a single block
        let nonces = HashMap::with_capacity(10_000);
        Self { tx_lists: HashMap::new(), nonces, parent_block: 0 }
    }

    /// Inserts a tx in the pool, overwriting any existing tx with the same nonce
    pub fn put(&mut self, tx: Arc<Order>, parent_block: u64) {
        let sender = *tx.sender();

        if parent_block != self.parent_block {
            if !self.nonces.is_empty() {
                // if we last updated the nonces for a different block, clear the cache and re-sim
                debug!(
                    old_parent = self.parent_block,
                    new_parent = parent_block,
                    "clearing nonce cache"
                );
                self.nonces.clear();
            }
        } else if let Some(state_nonce) = self.nonces.get(&sender) {
            if tx.nonce() < *state_nonce {
                debug!(state_nonce, tx_nonce = tx.nonce(), %sender, "discarding for nonce");
                return;
            }
        }

        self.tx_lists.entry(sender).or_insert(TxList::new(sender)).put(tx);
    }

    pub fn active_orders(&mut self, parent_block: u64) -> Option<ActiveOrders> {
        if parent_block != self.parent_block {
            self.nonces.clear();
            (!self.tx_lists.is_empty()).then_some(ActiveOrders::new(self.tx_lists.clone()))
        } else {
            let mut active = HashMap::new();

            for (sender, tx_list) in self.tx_lists.iter() {
                if let Some(state_nonce) = self.nonces.get(sender) {
                    if tx_list.has_nonce(state_nonce) {
                        // we have this nonce
                        let mut tx_list = tx_list.clone();
                        tx_list.forward(*state_nonce);

                        assert!(!tx_list.is_empty());

                        active.insert(*sender, tx_list);
                    }
                } else {
                    // we have a new sender
                    active.insert(*sender, tx_list.clone());
                }
            }

            (!active.is_empty()).then_some(ActiveOrders::new(active))
        }
    }

    /// Either we have these nonces or we have new senders
    pub fn get_new_active(&self, state_nonces: &StateNonces) -> Option<ActiveOrders> {
        let mut active = HashMap::new();

        for (sender, tx_list) in self.tx_lists.iter() {
            if let Some(state_nonce) = state_nonces.get(sender) {
                if tx_list.has_nonce(state_nonce) {
                    // we have this nonce
                    let mut tx_list = tx_list.clone();
                    tx_list.forward(*state_nonce);

                    assert!(!tx_list.is_empty());

                    active.insert(*sender, tx_list);
                }
            } else {
                // we have a new sender
                active.insert(*sender, tx_list.clone());
            }
        }

        (!active.is_empty()).then_some(ActiveOrders::new(active))
    }

    pub fn update_nonces(&mut self, state_nonces: StateNonces, parent_block: u64) {
        self.nonces.clear();
        self.parent_block = parent_block;

        for (sender, state_nonce) in state_nonces.0 {
            self.nonces.insert(sender, state_nonce);
            if let Some(tx_list) = self.tx_lists.get_mut(&sender) {
                if tx_list.forward(state_nonce) {
                    self.tx_lists.remove(&sender);
                }
            }
        }
    }

    /// Clear all mined nonces for each address (built block is now the parent)
    pub fn clear_mined(&mut self, txs: impl Iterator<Item = (Address, u64)>, block_number: u64) {
        self.nonces.clear();
        self.parent_block = block_number;

        for (sender, mined_nonce) in txs {
            let state_nonce = mined_nonce + 1;
            self.nonces.insert(sender, state_nonce);
            if let Some(tx_list) = self.tx_lists.get_mut(&sender) {
                if tx_list.forward(state_nonce) {
                    self.tx_lists.remove(&sender);
                }
            }
        }
    }
}

#[derive(Debug, Clone)]
pub struct TxList {
    sender: Address,
    txs: BTreeMap<u64, Arc<Order>>,
}

impl TxList {
    pub fn new(sender: Address) -> Self {
        Self { sender, txs: BTreeMap::new() }
    }

    pub fn sender(&self) -> &Address {
        &self.sender
    }

    pub fn put(&mut self, tx: Arc<Order>) {
        assert_eq!(self.sender, *tx.sender());
        self.txs.insert(tx.nonce(), tx);
    }

    pub fn has_nonce(&self, nonce: &u64) -> bool {
        self.txs.contains_key(nonce)
    }

    /// Removes all transactions with nonce lower than the next_nonce.
    /// Returns true if list becomes empty after removal.
    pub fn forward(&mut self, state_nonce: u64) -> bool {
        // TODO: fix this since it's sorted
        self.txs.retain(|nonce, _| *nonce >= state_nonce);
        self.txs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.txs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }

    pub fn first_ready(&mut self, state_nonce: Option<&u64>) -> Option<Arc<Order>> {
        if let Some(state_nonce) = state_nonce {
            self.txs.remove(state_nonce)
        } else {
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
