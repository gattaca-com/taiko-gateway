use std::collections::{BTreeMap, HashMap};

use alloy_consensus::Transaction;
use alloy_primitives::Address;
use pc_common::{sequencer::Order, utils::utcnow_sec};
use tracing::{debug, info};

use crate::{sorting::ActiveOrders, types::StateNonces};

pub struct TxPool {
    // sender -> tx list
    tx_lists: HashMap<Address, TxList>,
    // address -> state_nonce valid at block `parent_block` + 1
    nonces: StateNonces,
    valid_orders: bool,
    // this includes duplicates and invalid nonces
    discarded_orders: u64,
}

impl TxPool {
    pub fn new() -> Self {
        // this should be big enough for all the users in a single block
        Self {
            tx_lists: HashMap::new(),
            nonces: StateNonces::default(),
            valid_orders: false,
            discarded_orders: 0,
        }
    }

    /// Inserts a tx in the pool, overwriting any existing tx with the same nonce
    pub fn put(&mut self, tx: Order, parent_block: u64) {
        let sender = *tx.sender();

        if !self.nonces.is_valid_parent(parent_block) {
            if !self.nonces.is_empty() {
                // if we last updated the nonces for a different block, clear the cache and re-sim
                debug!(cache_block = self.nonces.valid_block, parent_block, "clearing nonce cache");
                self.nonces.clear();
            }
        } else if let Some(state_nonce) = self.nonces.get(&sender) {
            if tx.nonce() < *state_nonce {
                self.discarded_orders += 1;
                return;
            }
        }

        self.valid_orders = true;
        self.tx_lists.entry(sender).or_insert(TxList::new(sender)).put(tx);
    }

    /// Return the active orders, checking the nonces cache for the block number being built
    pub fn active_orders(&mut self, block_number: u64, base_fee: u128) -> Option<ActiveOrders> {
        if !self.nonces.is_valid_block(block_number) {
            debug!("invalid nonce cache, looking for valid orders");

            // nonces are stale, clear the cache and re-sim
            self.nonces.clear();
            (!self.tx_lists.is_empty()).then(|| {
                let tx_lists = self
                    .tx_lists
                    .iter()
                    .filter(|(_, tx_list)| tx_list.has_tx_by_fee(base_fee))
                    .map(|(sender, tx_list)| (*sender, tx_list.clone()))
                    .collect();

                ActiveOrders::new(tx_lists)
            })
        } else {
            debug!("valid nonce cache, getting active orders");

            // nonces are valid, get the active orders from those
            self.get_active_for_nonces(&self.nonces, base_fee)
        }
    }

    /// Return active orders: either we have these nonces or we have new senders
    pub fn get_active_for_nonces(
        &self,
        state_nonces: &StateNonces,
        base_fee: u128,
    ) -> Option<ActiveOrders> {
        let mut active = HashMap::with_capacity(self.tx_lists.len());

        for (sender, tx_list) in self.tx_lists.iter() {
            if let Some(state_nonce) = state_nonces.get(sender) {
                if tx_list.has_tx_by_nonce(state_nonce, base_fee) {
                    // we have this nonce and fee is high enough
                    let mut tx_list = tx_list.clone();
                    tx_list.forward(state_nonce);

                    assert!(!tx_list.is_empty());

                    active.insert(*sender, tx_list);
                }
            } else if tx_list.has_tx_by_fee(base_fee) {
                // new sender
                active.insert(*sender, tx_list.clone());
            }
        }

        (!active.is_empty()).then(|| ActiveOrders::new(active))
    }

    pub fn update_nonces(&mut self, state_nonces: StateNonces) {
        self.nonces = state_nonces;

        for (sender, state_nonce) in self.nonces.iter() {
            if let Some(tx_list) = self.tx_lists.get_mut(sender) {
                if tx_list.forward(state_nonce) {
                    self.tx_lists.remove(sender);
                }
            }
        }
    }

    /// Clear all mined nonces for each address (built block is now the parent)
    pub fn clear_mined(
        &mut self,
        mined_block: u64,
        mined_txs: impl Iterator<Item = (Address, u64)>,
    ) {
        self.discarded_orders = 0;
        let nonces = StateNonces::new_from_mined(mined_block, mined_txs);
        self.update_nonces(nonces);
    }

    pub fn clear_nonces(&mut self) {
        debug!("clearing state nonces");
        self.nonces.clear();
    }

    pub fn set_no_valid_orders(&mut self) {
        self.valid_orders = false;
    }

    pub fn has_valid_orders(&self) -> bool {
        self.valid_orders
    }

    pub fn report(&self, parent_block: u64) {
        if self.tx_lists.is_empty() {
            info!(discarded_orders = self.discarded_orders, "expty tx pool");
            return;
        }

        let random = utcnow_sec() as usize % self.tx_lists.len();
        let (address, tx_list) = self.tx_lists.iter().skip(random).next().unwrap();

        let cache_nonce =
            if self.nonces.is_valid_parent(parent_block) { self.nonces.get(address) } else { None };

        info!(
            discarded_orders = self.discarded_orders,
            senders = self.tx_lists.len(),
            ex_address =% address,
            ex_txs = tx_list.txs.len(),
            ex_next_nonce = tx_list.txs.first_key_value().map(|(nonce, _)| nonce).unwrap_or(&0),
            ex_cache_nonce = ?cache_nonce,
            "tx pool status"
        );
    }
}

#[derive(Debug, Clone)]
pub struct TxList {
    sender: Address,
    txs: BTreeMap<u64, Order>,
}

impl TxList {
    pub fn new(sender: Address) -> Self {
        Self { sender, txs: BTreeMap::new() }
    }

    pub fn sender(&self) -> &Address {
        &self.sender
    }

    pub fn put(&mut self, tx: Order) {
        assert_eq!(self.sender, *tx.sender());
        self.txs.insert(tx.nonce(), tx);
    }

    pub fn has_tx_by_nonce(&self, nonce: &u64, base_fee: u128) -> bool {
        self.txs.get(nonce).map(|tx| tx.max_fee_per_gas() >= base_fee).unwrap_or(false)
    }

    pub fn has_tx_by_fee(&self, base_fee: u128) -> bool {
        self.txs.first_key_value().map(|(_, tx)| tx.max_fee_per_gas() >= base_fee).unwrap_or(false)
    }

    /// Removes all transactions with nonce lower than the state_nonce.
    /// Returns true if list becomes empty after removal.
    pub fn forward(&mut self, state_nonce: &u64) -> bool {
        // TODO: fix this since it's sorted
        self.txs.retain(|nonce, _| nonce >= state_nonce);
        self.txs.is_empty()
    }

    pub fn len(&self) -> usize {
        self.txs.len()
    }

    pub fn is_empty(&self) -> bool {
        self.txs.is_empty()
    }

    pub fn first_ready(&mut self, state_nonce: Option<&u64>, base_fee: u128) -> Option<Order> {
        if let Some(state_nonce) = state_nonce {
            if let Some(tx) = self.txs.get(state_nonce) {
                if tx.max_fee_per_gas() >= base_fee {
                    return self.txs.remove(state_nonce);
                }
            }
        } else if self.has_tx_by_fee(base_fee) {
            return self.txs.pop_first().map(|(_, tx)| tx);
        }

        None
    }

    pub fn weight(&self) -> u128 {
        self.txs
            .first_key_value()
            .map(|(_, tx)| tx.priority_fee_or_price())
            .expect("txlist shouldn't be called when empty")
    }
}
