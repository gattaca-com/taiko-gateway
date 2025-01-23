use std::collections::{BTreeMap, HashMap};

use alloy_consensus::Transaction;
use alloy_primitives::Address;
use crossbeam_channel::Receiver;
use pc_common::sequencer::Order;

pub struct TxPool {
    mempool_rx: Receiver<Order>,
    // sender -> nonce -> txs
    txs: HashMap<Address, BTreeMap<u64, Order>>,
}

// TODO: improve this
impl TxPool {
    pub fn new(mempool_rx: Receiver<Order>) -> Self {
        Self { mempool_rx, txs: HashMap::new() }
    }

    pub fn fetch(&mut self) {
        for tx in self.mempool_rx.try_iter().take(50) {
            let sender = tx.recover_signer().unwrap();
            self.txs.entry(sender).or_default().insert(tx.nonce(), tx);
        }
    }

    // should pick highest nonce for each address
    pub fn clear_mined(&mut self, txs: impl Iterator<Item = (Address, u64)>) {
        for (sender, nonce) in txs {
            if let Some(txs) = self.txs.get_mut(&sender) {
                txs.retain(|_, tx| tx.nonce() > nonce);
            }
        }

        self.txs.retain(|_, txs| !txs.is_empty());
    }

    pub fn next_sequence(&mut self) -> Option<Order> {
        for txs in self.txs.values_mut() {
            if let Some((_, tx)) = txs.pop_first() {
                return Some(tx);
            }
        }

        None
    }
}
