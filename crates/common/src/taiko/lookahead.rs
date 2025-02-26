use std::{
    sync::Arc,
    time::{Duration, Instant},
};

use alloy_primitives::Address;
use alloy_provider::ProviderBuilder;
use alloy_sol_types::SolInterface;
use parking_lot::RwLock;
use tracing::{error, info, Instrument};
use url::Url;

use super::{pacaya::preconf::PreconfWhitelist::OperatorNotAvailableYet, PreconfWhitelist};
use crate::{
    beacon::BeaconHandle, config::LookaheadConfig, runtime::spawn,
    taiko::pacaya::preconf::PreconfWhitelist::PreconfWhitelistErrors, utils::utcnow_sec,
};

#[tracing::instrument(skip_all, name = "lookahead")]
pub async fn start_looahead_loop(
    l1_rpc: Url,
    whitelist_contract: Address,
    beacon_handle: BeaconHandle,
    config: LookaheadConfig,
) -> eyre::Result<LookaheadHandle> {
    let l1_provider = ProviderBuilder::new().disable_recommended_fillers().on_http(l1_rpc);
    let whitelist = PreconfWhitelist::new(whitelist_contract, l1_provider);

    let lookahead = Arc::new(RwLock::new(Lookahead::default()));

    let lookahead_rw = lookahead.clone();
    let err_operator_not_available_yet = alloy_primitives::hex::encode(
        PreconfWhitelistErrors::OperatorNotAvailableYet(OperatorNotAvailableYet {}).abi_encode(),
    );
    spawn(
        async move {
            loop {
                if beacon_handle.slot_in_epoch() < 29 {
                    match fetch_operators(&whitelist).await {
                        Ok((current_operator, next_operator)) => {
                            let current_epoch = beacon_handle.current_epoch();

                            {
                                let mut lookahead = lookahead_rw.write();
                                lookahead.curr = current_operator;
                                lookahead.curr_epoch = current_epoch;
                                lookahead.next = next_operator;
                                lookahead.next_epoch = current_epoch + 1;
                            }
                        }

                        Err(err) => {
                            // ignore  OperatorNotAvailableYet err
                            if !err.to_string().contains(&err_operator_not_available_yet) {
                                error!(%err, "failed to fetch operators");
                                std::process::exit(1);
                            }
                        }
                    }
                }

                tokio::time::sleep(Duration::from_secs(beacon_handle.seconds_per_slot / 3)).await;
            }
        }
        .in_current_span(),
    );

    while *lookahead.read() == Lookahead::default() {
        let current_slot = beacon_handle.current_slot();
        let current_epoch = beacon_handle.current_epoch();
        let slot_in_epoch = beacon_handle.slot_in_epoch();
        info!(slot_in_epoch, current_slot, current_epoch, "waiting to be initialized");
        tokio::time::sleep(Duration::from_secs(beacon_handle.seconds_per_slot / 2)).await;
    }

    let lookahead_rw = lookahead.clone();
    spawn(async move {
        loop {
            let lookahead = { *lookahead_rw.read() };
            let current_slot = beacon_handle.current_slot();
            let current_epoch = beacon_handle.current_epoch();
            let remaining_slots = beacon_handle.slots_per_epoch - beacon_handle.slot_in_epoch();

            let current_operator = lookahead.curr;
            let next_operator = lookahead.next;

            info!(current_slot, remaining_slots, current_epoch, %current_operator, %next_operator, "lookahead");

            tokio::time::sleep(Duration::from_secs(beacon_handle.seconds_per_slot / 2)).await;
        }
    }.in_current_span());

    Ok(LookaheadHandle::new(lookahead, config, beacon_handle))
}

async fn fetch_operators(whitelist: &PreconfWhitelist) -> eyre::Result<(Address, Address)> {
    let current_operator = whitelist.getOperatorForCurrentEpoch().call().await?._0;
    let next_operator = whitelist.getOperatorForNextEpoch().call().await?._0;

    Ok((current_operator, next_operator))
}

#[derive(Debug, Default, Clone, Copy, Eq, PartialEq)]
pub struct Lookahead {
    pub curr: Address,
    pub curr_epoch: u64,
    pub next: Address,
    pub next_epoch: u64,
}

#[derive(Debug, Clone)]
pub struct LookaheadHandle {
    lookahead: Arc<RwLock<Lookahead>>,
    checked: Instant,
    last: Lookahead,
    config: LookaheadConfig,
    beacon: BeaconHandle,
}

impl LookaheadHandle {
    fn new(
        lookahead: Arc<RwLock<Lookahead>>,
        config: LookaheadConfig,
        beacon: BeaconHandle,
    ) -> Self {
        let last = *lookahead.read();
        Self { lookahead, checked: Instant::now(), last, config, beacon }
    }

    fn maybe_refresh(&mut self) {
        if self.checked.elapsed() > Duration::from_millis(500) {
            self.last = *self.lookahead.read();
            self.checked = Instant::now();
        }
    }

    // Returns true if the operator can sequence based on the lookahead, the lookahead is only
    // updated 1 slot after the epoch starts, so we make an additional check
    pub fn can_sequence(&mut self, operator: &Address) -> (bool, &str) {
        self.maybe_refresh();
        let lookahead = &self.last;

        // current operator only sequences until here
        let cutoff_slot = self.beacon.slot_epoch_start(lookahead.curr_epoch) +
            self.beacon.slots_per_epoch -
            self.config.delay_sequence_slots;

        // next operator sequences after this time
        let cutoff_time = self.beacon.timestamp_of_slot(cutoff_slot) + self.config.buffer_secs;

        if self.beacon.current_epoch() == lookahead.next_epoch {
            // lookahead hasnt been updated yet
            if operator == &lookahead.next {
                return (true, "next operator after buffer_secs (waiting for lookahead update)");
            } else {
                return (false, "operator is not the next (waiting for lookahead update)");
            }
        };

        match (operator == &lookahead.curr, operator == &lookahead.next) {
            (true, true) => (true, "operator is both current and next"),
            (true, false) => {
                // either current operator before delay slots
                if self.beacon.current_slot() < cutoff_slot {
                    (true, "current operator before delay_slots")
                } else {
                    (false, "current operator but too late in epoch")
                }
            }
            (false, true) => {
                // or next operator after buffer_secs
                if utcnow_sec() > cutoff_time {
                    (true, "next operator after buffer_secs")
                } else {
                    (false, "next operator but too early in epoch")
                }
            }
            (false, false) => (false, "operator is not in lookahead"),
        }
    }

    pub fn can_propose(&mut self, operator: &Address) -> bool {
        self.maybe_refresh();
        let lookahead = &self.last;

        let is_operator = if self.beacon.current_epoch() == lookahead.next_epoch {
            // lookahead hasnt been updated yet
            operator == &lookahead.next
        } else {
            operator == &lookahead.curr
        };

        // this makes sure we dont propose right at the beginning of the epoch
        is_operator && self.beacon.slot_in_epoch() > self.config.delay_sequence_slots
    }

    // Clear all remaining batches if we're approaching a different operator turn
    pub fn should_clear_proposal(&mut self, operator: &Address) -> bool {
        self.maybe_refresh();
        let lookahead = &self.last;

        // current operator after delay slots
        let current_check = &lookahead.curr == operator &&
            (self.beacon.slot_in_epoch() >=
                self.beacon.slots_per_epoch - self.config.delay_sequence_slots);

        current_check && &lookahead.next != operator
    }
}
