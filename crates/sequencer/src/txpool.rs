use std::{
    collections::{BTreeMap, HashMap},
    sync::Arc,
};

use alloy_consensus::{Transaction, TxEnvelope};
use alloy_primitives::Address;
use crossbeam_channel::Receiver;
use pc_common::sequencer::Order;

pub struct TxPool {
    mempool_rx: Receiver<Arc<TxEnvelope>>,
    // sender -> nonce -> txs
    txs: HashMap<Address, BTreeMap<u64, Arc<TxEnvelope>>>,
}

// TODO: improve this
impl TxPool {
    pub fn new(mempool_rx: Receiver<Arc<TxEnvelope>>) -> Self {
        Self { mempool_rx, txs: HashMap::new() }
    }

    pub fn try_fetch_txs(&mut self) {
        for tx in self.mempool_rx.try_iter().take(50) {
            let sender = tx.recover_signer().unwrap();
            self.txs.entry(sender).or_default().insert(tx.nonce(), tx);
        }
    }

    // should pick highest nonce for each address
    pub fn clear_mined(&mut self, txs: impl Iterator<Item = (Address, u64)>) {
        for (sender, nonce) in txs {
            self.txs.get_mut(&sender).map(|txs| {
                txs.retain(|_, tx| tx.nonce() > nonce);
            });
        }

        self.txs.retain(|_, txs| !txs.is_empty());
    }

    pub fn next_sequence(&mut self) -> Option<Order> {
        for txs in self.txs.values_mut() {
            if let Some((_, tx)) = txs.pop_first() {
                return Some(Order::Tx(tx));
            }
        }

        None
    }

    pub fn has_txs(&self) -> bool {
        self.txs.values().any(|txs| !txs.is_empty())
    }
}
