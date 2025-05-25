use std::{
    collections::{HashMap, VecDeque},
    time::{Duration, Instant},
};

use alloy_consensus::Transaction;
use alloy_primitives::{utils::format_ether, Address};
use pc_common::{
    metrics::SequencerMetrics,
    sequencer::{ExecutionResult, InvalidReason, Order, StateId},
    taiko::pacaya::estimate_compressed_size,
};
use tracing::{debug, info, warn};

use crate::{
    simulator::SimulatorClient,
    tx_pool::TxList,
    types::{BlockInfo, SimulatedOrder, StateNonces, ValidOrder},
};

#[derive(Debug, Clone)]
pub struct SortData {
    /// Block info
    pub block_info: BlockInfo,
    /// State id for next transactions or to be sealed
    pub state_id: StateId,
    /// Time when we started building the block, excluding the anchor
    pub start_build: Instant,
    /// Mempool snapshot when the started building the block
    pub active_orders: ActiveOrders,
    /// Simulated nonces, sender -> state nonce
    pub state_nonces: StateNonces,
    /// All orders in the current block (excluding the anchor)
    pub orders: Vec<Order>,
    /// Cumulative size of the sequenced txs, rlp encoded
    pub uncompressed_size: usize,
    /// Time when we should stop sequencing this block
    target_seal: Instant,
    /// Cumulative builder payment
    builder_payment: u128,
    /// Cumulative gas used in the block
    gas_remaining: u128,
    /// Cumulative size of the block
    max_block_size: usize,
    /// Next best order (simulated on the current block_info state_id), when all sims come back
    /// (in_flight_sims == 0) we update the block info with this state id
    next_best: Option<ValidOrder>,
    /// Number of sims in flight, wait until this is 0 before sending next batch of sims
    in_flight_sims: usize,
    /// Telemetry for the current block being built
    telemetry: SortingTelemetry,
    /// Whether we should seal the block because we reached some limits
    should_seal: bool,
    /// Whether we should discard the current block and restart
    should_discard: bool,
    /// When we last logged info about current block
    last_logged: Instant,
}

#[derive(Debug, Clone)]
pub struct ActiveOrders {
    tx_lists: VecDeque<TxList>,
    /// Number of all pending txs in tx lists
    num_txs: usize,
}

impl ActiveOrders {
    pub fn new(txs: HashMap<Address, TxList>) -> Self {
        let mut tx_lists = txs.into_values().collect::<Vec<_>>();

        tx_lists.sort_unstable_by_key(|tx| std::cmp::Reverse(tx.weight()));
        let num_txs = tx_lists.iter().map(|tx| tx.len()).sum();
        Self { tx_lists: tx_lists.into(), num_txs }
    }

    pub fn put(&mut self, order: Order) {
        let sender = order.sender();
        if let Some(tx_list) = self.tx_lists.iter_mut().find(|tx_list| tx_list.sender() == sender) {
            tx_list.put(order);
        } else {
            let mut tx_list = TxList::new(*sender);
            tx_list.put(order);
            self.tx_lists.push_back(tx_list);
        }
    }

    /// Returns the total number of txs in the active orders
    pub fn active_txs(&self) -> usize {
        self.num_txs
    }

    pub fn active_senders(&self) -> usize {
        self.tx_lists.len()
    }

    /// Removes a sender tx_list from the active orders
    pub fn remove_sender(&mut self, sender: &Address) {
        if let Some(idx) = self.tx_lists.iter().position(|tx_list| tx_list.sender() == sender) {
            self.tx_lists.remove(idx);
        }
    }

    fn get_next_best<'a>(
        &'a mut self,
        max: usize,
        state_nonces: &'a HashMap<Address, u64>,
        base_fee: u128,
    ) -> impl Iterator<Item = Order> + 'a {
        self.tx_lists
            .iter_mut()
            .filter_map(move |tx| {
                let state_nonce = state_nonces.get(tx.sender());
                tx.first_ready(state_nonce, base_fee)
            })
            .take(max)
    }
}

impl SortData {
    pub fn new(
        block_info: BlockInfo,
        anchor_state_id: StateId,
        active_orders: ActiveOrders,
        gas_limit: u128,
        target_block_time: Duration,
        max_block_size: usize,
    ) -> Self {
        Self {
            block_info,
            state_id: anchor_state_id,
            start_build: Instant::now(),
            target_seal: Instant::now() + target_block_time,
            builder_payment: 0,
            gas_remaining: gas_limit,
            max_block_size,
            orders: Vec::new(),
            next_best: None,
            in_flight_sims: 0,
            active_orders,
            telemetry: SortingTelemetry::default(),
            state_nonces: StateNonces::new(block_info.block_number),
            should_seal: false,
            should_discard: false,
            last_logged: Instant::now(),
            uncompressed_size: 0,
        }
    }

    /// Apply the next best order
    pub fn maybe_apply_next(&mut self) {
        if let Some(next_best) = std::mem::take(&mut self.next_best) {
            assert!(self.is_valid(next_best.origin_state_id));
            assert!(self.gas_remaining >= next_best.gas_used);

            self.state_nonces.insert(*next_best.order.sender(), next_best.order.nonce() + 1);
            self.state_id = next_best.new_state_id;
            self.builder_payment += next_best.builder_payment;
            self.gas_remaining -= next_best.gas_used;
            self.uncompressed_size += next_best.order.raw().len();
            self.orders.push(next_best.order);

            let compressed = estimate_compressed_size(self.uncompressed_size);
            if compressed > self.max_block_size {
                warn!("exceeding block size, sealing early");
                self.should_seal = true;
            }
        }

        if self.last_logged.elapsed() > Duration::from_millis(500) {
            debug!(
                txs = self.num_txs(),
                builder_payment = format_ether(self.builder_payment),
                gas_remaining = self.gas_remaining,
                "sorting"
            );
            self.last_logged = Instant::now();
        }
    }

    /// Handle a new simulation result
    pub fn handle_sim(&mut self, sim: SimulatedOrder) {
        if !self.is_valid(sim.origin_state_id) {
            return;
        }

        self.in_flight_sims -= 1;
        self.telemetry.tot_sim_time += sim.sim_time;

        self.record_execution_result(&sim);
        let (state_id, gas_used, builder_payment) = match sim.execution_result {
            ExecutionResult::Success { state_id, gas_used, builder_payment } => {
                (state_id, gas_used, builder_payment)
            }
            ExecutionResult::Revert { state_id, gas_used, builder_payment } => {
                (state_id, gas_used, builder_payment)
            }
            ExecutionResult::Invalid { reason } => {
                match reason {
                    InvalidReason::NonceTooLow { tx: _, state } => {
                        // warn!(tx, state, sender = ?sim.order.sender(), "nonce too low, updating
                        // nonce cache");
                        let sender = *sim.order.sender();
                        self.state_nonces.insert(sender, state);
                    }
                    InvalidReason::NonceTooHigh { tx: _, state } => {
                        // warn!(tx, state, sender = ?sim.order.sender(), "nonce too high, updating
                        // nonce cache");
                        let sender = *sim.order.sender();
                        self.state_nonces.insert(sender, state);
                    }
                    InvalidReason::NotEnoughGas => {
                        warn!("not enough gas, sealing early (this should be very rare)");
                        self.should_seal = true;
                    }
                    InvalidReason::CapLessThanBaseFee => {
                        warn!("max fee per gas less than block base fee, this should not happen!");
                        let sender = *sim.order.sender();
                        self.state_nonces.insert(sender, sim.order.nonce());
                        debug_assert!(false)
                    }
                    InvalidReason::InsufficientFunds { .. } => {
                        // warn!(have, want, sender = ?sim.order.sender(), "insufficient funds,
                        // removing sender for this block
                        let sender = *sim.order.sender();
                        self.state_nonces.insert(sender, sim.order.nonce());
                        self.active_orders.remove_sender(&sender);
                    }
                    InvalidReason::StateIdNotFound { state } => {
                        warn!(state, "state id not found! this should not happen, are multiple sequencers running?");
                        self.should_discard = true;
                    }
                    InvalidReason::Uknown(reason) => {
                        warn!(reason, "unhandled invalid simulation");
                    }
                }
                return;
            }
        };

        if gas_used > self.gas_remaining {
            // TODO: remove from sender
            return;
        }

        let tx_to_put_back =
            if self.next_best.as_ref().is_none_or(|t| t.builder_payment < builder_payment) {
                let valid_order = ValidOrder {
                    origin_state_id: sim.origin_state_id,
                    new_state_id: state_id,
                    gas_used,
                    builder_payment,
                    order: sim.order,
                };
                self.next_best.replace(valid_order).map(|prev_best| prev_best.order)
            } else {
                Some(sim.order)
            };

        if let Some(tx) = tx_to_put_back {
            self.active_orders.put(tx)
        }
    }

    pub fn maybe_sim_next_batch(&mut self, simulator: &SimulatorClient, max_sims_per_loop: usize) {
        if self.in_flight_sims > 0 || self.should_seal() {
            return;
        }

        // Apply the next best order if there is one
        self.maybe_apply_next();

        for order in self.active_orders.get_next_best(
            max_sims_per_loop,
            &self.state_nonces,
            self.block_info.base_fee,
        ) {
            self.in_flight_sims += 1;
            self.telemetry.n_sims_sent += 1;
            simulator.spawn_sim_tx(order, self.state_id);
        }
    }

    pub fn is_simulating(&self) -> bool {
        self.in_flight_sims > 0
    }

    pub fn is_valid(&self, state_id: StateId) -> bool {
        self.state_id == state_id
    }

    pub fn set_should_seal(&mut self) {
        self.should_seal = true;
    }

    pub fn should_seal(&self) -> bool {
        self.should_seal || self.is_past_target_seal()
    }

    pub fn should_discard(&self) -> bool {
        self.should_discard
    }

    pub fn is_past_target_seal(&self) -> bool {
        Instant::now() > self.target_seal
    }

    pub fn num_txs(&self) -> usize {
        self.orders.len()
    }

    fn record_execution_result(&mut self, sim: &SimulatedOrder) {
        match &sim.execution_result {
            ExecutionResult::Success { .. } => {
                self.telemetry.n_sims_success += 1;
                SequencerMetrics::record_sim_success();
            }
            ExecutionResult::Revert { .. } => {
                self.telemetry.n_sims_revert += 1;
                SequencerMetrics::record_sim_revert();
            }
            ExecutionResult::Invalid { reason } => {
                if matches!(reason, InvalidReason::Uknown(_)) {
                    SequencerMetrics::record_sim_unknown();
                } else {
                    SequencerMetrics::record_sim_invalid();
                }

                match reason {
                    InvalidReason::NonceTooLow { .. } => {
                        self.telemetry.n_sims_invalid_nonce_too_low += 1;
                    }
                    InvalidReason::NonceTooHigh { .. } => {
                        self.telemetry.n_sims_invalid_nonce_too_high += 1;
                    }
                    InvalidReason::InsufficientFunds { .. } => {
                        self.telemetry.n_sims_invalid_insufficient_funds += 1;
                    }

                    _ => {
                        self.telemetry.n_sims_invalid_other += 1;
                    }
                }
            }
        }
    }

    pub fn report(&self) {
        self.telemetry.report();
    }
}

#[derive(Debug, Clone, Default)]
pub struct SortingTelemetry {
    n_sims_sent: usize,
    n_sims_success: usize,
    n_sims_revert: usize,
    n_sims_invalid_nonce_too_low: usize,
    n_sims_invalid_nonce_too_high: usize,
    n_sims_invalid_insufficient_funds: usize,
    n_sims_invalid_other: usize,
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

        let invalid_rate_nonce_too_low =
            get_rate(self.n_sims_invalid_nonce_too_low, self.n_sims_sent);
        let invalid_rate_nonce_too_high =
            get_rate(self.n_sims_invalid_nonce_too_high, self.n_sims_sent);
        let invalid_rate_insufficient_funds =
            get_rate(self.n_sims_invalid_insufficient_funds, self.n_sims_sent);
        let invalid_rate_other = get_rate(self.n_sims_invalid_other, self.n_sims_sent);
        let received_rate = success_rate +
            revert_rate +
            invalid_rate_nonce_too_low +
            invalid_rate_nonce_too_high +
            invalid_rate_insufficient_funds +
            invalid_rate_other;

        let stale_rate = 100.0 - received_rate; // we didnt get these back
        let stale_rate = (stale_rate * 100.0).round() / 100.0;

        let avg_sim_time = if received_rate > 0.0 && self.n_sims_sent > 0 {
            self.tot_sim_time / self.n_sims_sent as u32 / (received_rate * 100.0).round() as u32 *
                10000
        } else {
            Duration::from_secs(0)
        };

        info!(
            n_sims_sent = self.n_sims_sent,
            success_rate,
            revert_rate,
            invalid_rate_nonce_too_low,
            invalid_rate_nonce_too_high,
            invalid_rate_insufficient_funds,
            invalid_rate_other,
            stale_rate,
            avg_sim_time =? avg_sim_time,
            tot_sim_time =? self.tot_sim_time,
            "sorting telemetry",
        );
    }
}
