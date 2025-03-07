use std::{
    collections::{HashMap, VecDeque},
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_consensus::Transaction;
use alloy_primitives::{utils::format_ether, Address};
use pc_common::sequencer::{ExecutionResult, InvalidReason, Order, StateId};
use tracing::{debug, info, warn};

use crate::{
    simulator::SimulatorClient,
    tx_pool::{TxList, TxPool},
    types::{BlockInfo, SimulatedOrder, ValidOrder},
};

#[derive(Debug, Clone)]
pub struct SortData {
    /// Block info
    pub block_info: BlockInfo,
    /// State id for next transactions or to be sealed
    pub state_id: StateId,
    /// Time when we started building the block, excluding the anchor
    pub start_build: Instant,
    /// Time when we should stop sequencing this block
    pub target_seal: Instant,
    /// Builder payment
    pub builder_payment: u128,
    /// Running total of gas used in the block
    pub gas_remaining: u128,
    /// number of user txs in the block
    pub num_txs: usize,
    /// Next best order (simulated on the current block_info state_id), when all sims come back
    /// (in_flight_sims == 0) we update the block info with this state id
    pub next_best: Option<ValidOrder>,
    /// Number of sims in flight, wait until this is 0 before sending next batch of sims
    pub in_flight_sims: usize,
    /// Mempool snapshot when the started building the block
    pub active_orders: ActiveOrders,
    /// Telemetry for the current block being built
    pub telemetry: SortingTelemetry,
}

#[derive(Debug, Clone)]
pub struct ActiveOrders(VecDeque<TxList>);

impl ActiveOrders {
    pub fn new(txs: &HashMap<Address, TxList>) -> Self {
        let mut active = txs.clone().into_values().collect::<Vec<_>>();
        active.sort_unstable_by_key(|tx| std::cmp::Reverse(tx.weight()));
        Self(active.into())
    }

    pub fn put(&mut self, order: Arc<Order>) {
        let sender = order.sender();
        if let Some(tx_list) = self.0.iter_mut().find(|tx_list| tx_list.sender() == sender) {
            tx_list.put(order);
        } else {
            let mut tx_list = TxList::new(*sender, None);
            tx_list.put(order);
            self.0.push_back(tx_list);
        }
    }

    pub fn update_next_nonce(&mut self, order: &Order) {
        if let Some(tx_list) = self.0.iter_mut().find(|tx_list| tx_list.sender() == order.sender())
        {
            tx_list.next_nonce = Some(order.nonce() + 1);
        }
    }

    /// Returns the total number of txs in the active orders
    pub fn active_txs(&self) -> usize {
        self.0.iter().map(|tx_list| tx_list.len()).sum()
    }

    pub fn active_senders(&self) -> usize {
        self.0.len()
    }

    /// Clear senders with no orders left
    pub fn clear_senders(&mut self) {
        self.0.retain(|tx_list| tx_list.len() > 0);
    }

    pub fn get_next_best(&mut self, max: usize) -> impl Iterator<Item = Arc<Order>> + '_ {
        self.0.iter_mut().filter_map(|tx| tx.first_ready()).take(max)
    }
}

impl SortData {
    pub fn new(
        block_info: BlockInfo,
        anchor_state_id: StateId,
        active_orders: ActiveOrders,
        gas_limit: u128,
        target_block_time: Duration,
    ) -> Self {
        Self {
            block_info,
            state_id: anchor_state_id,
            start_build: Instant::now(),
            target_seal: Instant::now() + target_block_time,
            builder_payment: 0,
            gas_remaining: gas_limit,
            num_txs: 0,
            next_best: None,
            in_flight_sims: 0,
            active_orders,
            telemetry: SortingTelemetry::default(),
        }
    }

    /// Apply the next best order
    pub fn maybe_apply_next(&mut self) {
        if let Some(next_best) = std::mem::take(&mut self.next_best) {
            assert!(self.is_valid(next_best.origin_state_id));
            assert!(self.gas_remaining >= next_best.gas_used);
            debug!(origin = ?next_best.origin_state_id, new = ?next_best.new_state_id, tx_hash = ?next_best.order.tx_hash(), "applying next best");

            self.active_orders.update_next_nonce(&next_best.order);

            self.state_id = next_best.new_state_id;
            self.builder_payment += next_best.builder_payment;
            self.gas_remaining -= next_best.gas_used;
            self.num_txs += 1;
        }
    }

    /// Handle a new simulation result
    pub fn handle_sim(&mut self, sim: SimulatedOrder, tx_pool: &mut TxPool) {
        if !self.is_valid(sim.origin_state_id) {
            warn!("sim is not valid!");
            return;
        }

        self.in_flight_sims -= 1;
        self.telemetry.tot_sim_time += sim.sim_time;

        let (state_id, gas_used, builder_payment) = match sim.execution_result {
            ExecutionResult::Success { state_id, gas_used, builder_payment } => {
                self.telemetry.n_sims_success += 1;
                (state_id, gas_used, builder_payment)
            }
            ExecutionResult::Revert { state_id, gas_used, builder_payment } => {
                self.telemetry.n_sims_revert += 1;
                (state_id, gas_used, builder_payment)
            }
            ExecutionResult::Invalid { reason } => {
                self.telemetry.n_sims_invalid += 1;

                match reason {
                    InvalidReason::NonceTooLow { tx, state } => {
                        warn!(tx, state, "nonce too low, removing from sender");
                        let sender = *sim.order.sender();
                        tx_pool.clear_mined(std::iter::once((sender, state.saturating_sub(1))));
                    }
                    InvalidReason::NonceTooHigh { tx, state } => {
                        warn!(tx, state, "nonce too high, removing from sender");
                        let sender = *sim.order.sender();
                        tx_pool.clear_mined(std::iter::once((sender, state.saturating_sub(1))));
                    }
                    InvalidReason::Other(reason) => {
                        warn!(reason, "unknown reason");
                    }
                }

                return;
            }
        };

        if gas_used > self.gas_remaining {
            // TODO: remove from sender
            return;
        }

        let tx_to_put_back = if self
            .next_best
            .as_ref()
            .is_none_or(|t| t.builder_payment < builder_payment)
        {
            let valid_order = ValidOrder {
                origin_state_id: sim.origin_state_id,
                new_state_id: state_id,
                gas_used,
                builder_payment,
                order: sim.order,
            };

            debug!(origin = ?valid_order.origin_state_id, new = ?valid_order.new_state_id, payment = format_ether(valid_order.builder_payment), hash = ?valid_order.order.tx_hash(), "next best");
            self.next_best.replace(valid_order).map(|prev_best| prev_best.order)
        } else {
            Some(sim.order)
        };

        if let Some(tx) = tx_to_put_back {
            self.active_orders.put(tx)
        }
    }

    pub fn maybe_sim_next_batch(&mut self, simulator: &SimulatorClient, max_sims_per_loop: usize) {
        if self.in_flight_sims > 0 {
            return;
        }

        // Apply the next best order if there is one
        self.maybe_apply_next();

        self.active_orders.clear_senders();
        if self.active_orders.active_senders() == 0 {
            debug!("exhausted active senders");
            return;
        }

        for order in self.active_orders.get_next_best(max_sims_per_loop) {
            self.in_flight_sims += 1;
            self.telemetry.n_sims_sent += 1;
            simulator.spawn_sim_tx(order, self.state_id);
        }
    }

    pub fn is_valid(&self, state_id: StateId) -> bool {
        self.state_id == state_id
    }

    pub fn should_seal(&self) -> bool {
        self.num_txs > 0 && Instant::now() > self.target_seal
    }

    pub fn should_reset(&self) -> bool {
        // went through all txs but none were actually valid
        self.num_txs == 0 && (self.in_flight_sims == 0 || Instant::now() > self.target_seal)
    }
}

#[derive(Debug, Clone, Default)]
pub struct SortingTelemetry {
    n_sims_sent: usize,
    n_sims_success: usize,
    n_sims_revert: usize,
    n_sims_invalid: usize,
    tot_sim_time: Duration,
}

impl SortingTelemetry {
    pub fn report(&self) {
        fn get_rate(n: usize, total: usize) -> f64 {
            if total == 0 {
                0.0
            } else {
                (n * 10000 / total) as f64 / 100.0
            }
        }

        // if the telemetry was created, we sent at least one sim
        let success_rate = get_rate(self.n_sims_success, self.n_sims_sent);
        let revert_rate = get_rate(self.n_sims_revert, self.n_sims_sent);
        let invalid_rate = get_rate(self.n_sims_invalid, self.n_sims_sent);
        let stale_rate = 100.0 - success_rate - revert_rate - invalid_rate; // we didnt get these back

        info!(
            n_sims_sent = self.n_sims_sent,
            success_rate,
            revert_rate,
            invalid_rate,
            stale_rate,
            tot_sim_time =? self.tot_sim_time,
            "sorting telemetry",
        );
    }
}
