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

    let current_operator = whitelist.getOperatorForCurrentEpoch().call().await?._0;
    let next_operator =
        whitelist.getOperatorForNextEpoch().call().await.map(|res| res._0).unwrap_or_default();

    let lookahead = Arc::new(RwLock::new(Lookahead {
        curr: current_operator,
        next: next_operator,
        updated_epoch: beacon_handle.current_slot(),
    }));

    let lookahead_rw = lookahead.clone();
    let err_operator_not_available_yet = alloy_primitives::hex::encode(
        PreconfWhitelistErrors::OperatorNotAvailableYet(OperatorNotAvailableYet {}).abi_encode(),
    );
    spawn(
        async move {
            let mut last_slot = 0;
            loop {
                if beacon_handle.slot_in_epoch() == 0 {
                    // avoid fetching on the first slot of the epoch in case it was a missed slot, 
                    // otherwise the previous "current" gets tagged as the new "current" for this epoch

                    continue;
                }

                let current_operator = match whitelist.getOperatorForCurrentEpoch().call().await {
                    Ok(res) => res._0,
                    Err(err) => {
                        // current operator should always be available
                        error!(%err, "failed to fetch current operator");
                        std::process::exit(1);
                    }
                };

                let next_operator = match whitelist.getOperatorForNextEpoch().call().await {
                    Ok(res) => Some(res._0),
                    Err(err) if err.to_string().contains(&err_operator_not_available_yet) => {
                        // next operator not available early in the slot
                        None
                    }

                    Err(err) => {
                        // exit for other errors
                        error!(%err, "failed to fetch current operator");
                        std::process::exit(1);
                    }
                }
                .unwrap_or_default();


                let current_slot = beacon_handle.current_slot();
                let current_epoch = beacon_handle.current_epoch();

                if last_slot != current_slot {
                    let remaining_slots = beacon_handle.slots_per_epoch - beacon_handle.slot_in_epoch();
                    info!(remaining_slots, current_slot, current_epoch, %current_operator, %next_operator);
                    last_slot = current_slot;
                }

                {
                    let mut lookahead = lookahead_rw.write();
                    lookahead.updated_epoch = current_epoch;
                    lookahead.curr = current_operator;
                    lookahead.next = next_operator;
                }

                tokio::time::sleep(Duration::from_secs(beacon_handle.seconds_per_slot / 3)).await;
            }
        }
        .in_current_span(),
    );

    Ok(LookaheadHandle::new(lookahead, config, beacon_handle))
}

#[derive(Debug, Clone, Copy)]
pub struct Lookahead {
    pub curr: Address,
    pub next: Address,
    pub updated_epoch: u64,
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

        if self.beacon.current_epoch() != lookahead.updated_epoch {
            // we're very early in the epoch, and current operator hasnt been updated yet
            if operator == &lookahead.next {
                return (true, "next operator after buffer_secs (waiting for lookahead update)");
            } else {
                return (false, "operator is not the next (waiting for lookahead update)");
            }
        }

        // current operator only sequences until here
        let cutoff_slot = self.beacon.slot_epoch_start(lookahead.updated_epoch) +
            self.beacon.slots_per_epoch -
            self.config.delay_sequence_slots;

        // next operator sequences after this time
        let cutoff_time = self.beacon.timestamp_of_slot(cutoff_slot) + self.config.buffer_secs;

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

    pub fn can_propose(&mut self, operator: &Address) -> (bool, &str) {
        self.maybe_refresh();
        let lookahead = &self.last;

        if self.beacon.current_epoch() != lookahead.updated_epoch {
            // we're very early in the epoch, and current operator hasnt been updated yet
            if operator == &lookahead.next {
                return (true, "current operator (waiting for lookahead update)");
            } else {
                return (false, "not the current operator (waiting for lookahead update)");
            }
        }

        if operator == &lookahead.curr {
            (true, "current operator")
        } else {
            (false, "not the current operator")
        }
    }

    // Clear all remaining batches if we're approaching a different operator turn
    pub fn should_clear_proposal(&mut self, operator: &Address) -> (bool, &str) {
        self.maybe_refresh();
        let lookahead = &self.last;

        if self.beacon.current_epoch() != lookahead.updated_epoch {
            assert!(self.beacon.slot_in_epoch() < 5);
            return (false, "stale lookahead (early in epoch)");
        };

        if operator != &lookahead.curr {
            return (false, "operator is not current");
        }

        if operator == &lookahead.next {
            return (false, "operator is both current and next");
        }

        // current operator only sequences until here
        let cutoff_slot = self.beacon.slot_epoch_start(lookahead.updated_epoch) +
            self.beacon.slots_per_epoch -
            self.config.delay_sequence_slots;

        if self.beacon.current_slot() < cutoff_slot {
            (false, "current operator early in epoch")
        } else {
            (true, "different operator turn approaching")
        }
    }
}
