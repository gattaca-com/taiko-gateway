use std::{
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use alloy_primitives::Address;
use alloy_provider::ProviderBuilder;
use parking_lot::RwLock;
use tracing::{debug, error, info, warn, Instrument};
use url::Url;

use super::PreconfWhitelist;
use crate::{
    beacon::BeaconHandle,
    config::LookaheadConfig,
    runtime::spawn,
    taiko::pacaya::preconf::PreconfWhitelist::PreconfWhitelistErrors,
    utils::{extract_revert_reason, utcnow_sec},
};

#[tracing::instrument(skip_all, name = "lookahead")]
pub async fn start_looahead_loop(
    l1_rpc: Url,
    whitelist_contract: Address,
    beacon_handle: BeaconHandle,
    config: LookaheadConfig,
    last_block_number: Arc<AtomicU64>,
) -> eyre::Result<LookaheadHandle> {
    let l1_provider = ProviderBuilder::new().disable_recommended_fillers().on_http(l1_rpc);
    let whitelist = PreconfWhitelist::new(whitelist_contract, l1_provider);

    let curr = whitelist.getOperatorForCurrentEpoch().call().await?._0;
    let next_operator =
        whitelist.getOperatorForNextEpoch().call().await.map(|res| res._0).unwrap_or_default();

    let lookahead = Arc::new(RwLock::new(Lookahead {
        curr,
        next: next_operator,
        updated_epoch: beacon_handle.current_epoch(),
    }));

    let lookahead_rw = lookahead.clone();

    spawn(
        async move {
            let mut seen_block_number = 0;
            let mut last_slot = 0;

            loop {
                tokio::time::sleep(Duration::from_secs(beacon_handle.seconds_per_slot / 3)).await;

                let block_number = last_block_number.load(Ordering::Relaxed);
                let current_slot = beacon_handle.current_slot();

                // avoid fetching on missed slots, otherwise the previous "current" can get tagged
                // as the new "current" for this epoch
                if block_number == seen_block_number {
                    // leave some grace period for the block to arrive
                    if last_slot != current_slot &&
                        utcnow_sec() - beacon_handle.timestamp_of_slot(current_slot) > 6
                    {
                        debug!(
                            slot = current_slot,
                            block = block_number,
                            "possible missed slot detected"
                        );
                        last_slot = current_slot;
                    }

                    continue;
                }

                seen_block_number = block_number;
                last_slot = current_slot;

                let current_operator = match whitelist.getOperatorForCurrentEpoch().call().await {
                    Ok(res) => res._0,
                    Err(err) => {
                        if let Some(err) =
                            extract_revert_reason::<PreconfWhitelistErrors>(&err.to_string())
                        {
                            match err {
                                PreconfWhitelistErrors::InvalidOperatorCount(_) => {
                                    warn!("whitelist is empty");
                                    Address::ZERO
                                }

                                _ => {
                                    let msg = format!("unhandled whitelist error: {err:?}");
                                    error!("{msg:?}");
                                    panic!("{msg}");
                                }
                            }
                        } else {
                            let msg = format!("failed to fetch current operator: {}", err);
                            error!("{msg:?}");
                            panic!("{msg}");
                        }
                    }
                };

                let next_operator = match whitelist.getOperatorForNextEpoch().call().await {
                    Ok(res) => res._0,
                    Err(err) => {
                        if let Some(err) =
                            extract_revert_reason::<PreconfWhitelistErrors>(&err.to_string())
                        {
                            match err {
                                PreconfWhitelistErrors::OperatorNotAvailableYet(_)
                                | PreconfWhitelistErrors::InvalidOperatorCount(_) => {
                                    Address::ZERO
                                }

                                _ => {
                                    let msg = format!("unhandled whitelist error: {err:?}");
                                    error!("{msg:?}");
                                    panic!("{msg}");
                                }
                            }
                        } else {
                            let msg = format!("failed to fetch current operator: {}", err);
                            error!("{msg:?}");
                            panic!("{msg}");
                        }

                    }
                };

                let current_epoch = beacon_handle.current_epoch();
                let remaining_slots = beacon_handle.slots_per_epoch - beacon_handle.slot_in_epoch();
                info!(remaining_slots, current_slot, current_epoch, %current_operator, %next_operator);

                {
                    let mut lookahead = lookahead_rw.write();
                    lookahead.updated_epoch = current_epoch;
                    lookahead.curr = current_operator;
                    lookahead.next = next_operator;
                }
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
            self.config.handover_window_slots;

        // next operator sequences after this time
        let cutoff_time = self.beacon.timestamp_of_slot(cutoff_slot) +
            self.config.handover_start_buffer_ms / 1_000;

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
            self.config.handover_window_slots;

        if self.beacon.current_slot() < cutoff_slot {
            (false, "current operator early in epoch")
        } else {
            (true, "different operator turn approaching")
        }
    }
}
