use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        Arc,
    },
    time::{Duration, Instant},
};

use alloy_primitives::Address;
use alloy_provider::{Provider, ProviderBuilder};
use parking_lot::RwLock;
use tracing::{debug, error, info, warn, Instrument};
use url::Url;

use super::{pacaya::wrapper::TaikoWrapper, PreconfWhitelist};
use crate::{
    beacon::BeaconHandle,
    config::{L2ChainConfig, LookaheadConfig},
    runtime::spawn,
    taiko::pacaya::preconf::PreconfWhitelist::PreconfWhitelistErrors,
    utils::{extract_revert_reason, utcnow_sec},
};

#[tracing::instrument(skip_all, name = "lookahead")]
pub async fn start_lookahead_loop(
    l1_rpc: Url,
    l2_chain_config: &L2ChainConfig,
    beacon_handle: BeaconHandle,
    config: LookaheadConfig,
    last_block_number: Arc<AtomicU64>,
) -> eyre::Result<LookaheadHandle> {
    let l1_provider = ProviderBuilder::new().disable_recommended_fillers().on_http(l1_rpc);
    let whitelist = PreconfWhitelist::new(l2_chain_config.whitelist_contract, l1_provider.clone());
    let wrapper = TaikoWrapper::new(l2_chain_config.wrapper_contract, l1_provider);

    let preconfs_enabled = Arc::new(AtomicBool::new(false));

    let preconfs_enabled_clone = preconfs_enabled.clone();
    tokio::spawn(async move {
        let mut current_router = Address::ZERO;
        loop {
            match wrapper.preconfRouter().call().await {
                Ok(address) => {
                    if address._0 != current_router {
                        if address._0 == Address::ZERO {
                            warn!("preconfs are now disabled in the wrapper");
                        } else {
                            warn!(router = %address._0, "preconfs are now enabled in the wrapper");
                        }

                        current_router = address._0;
                        preconfs_enabled.store(address._0 != Address::ZERO, Ordering::Relaxed);
                    }
                }
                Err(err) => {
                    error!(%err, "failed to fetch preconf router");
                    preconfs_enabled.store(false, Ordering::Relaxed);
                }
            }

            tokio::time::sleep(Duration::from_secs(12)).await;
        }
    });

    loop {
        // make sure we have at least a block in the current epoch, otherwise initial "current"
        // might be wrong
        let current_epoch = beacon_handle.current_epoch();
        let first_slot = beacon_handle.slot_epoch_start(current_epoch);
        let last_block = whitelist
            .provider()
            .get_block(alloy_eips::BlockId::latest(), false.into())
            .await?
            .expect("missing last block in lookahead!");
        let last_slot = beacon_handle.slot_for_timestamp(last_block.header.timestamp);

        if last_slot > first_slot {
            break;
        }

        warn!(
            first_slot,
            last_slot,
            current_epoch,
            "first or only missed slots since current epoch start, wait to fetch initial lookahead"
        );
        tokio::time::sleep(Duration::from_secs(12)).await;
    }

    let curr = whitelist.getOperatorForCurrentEpoch().call().await?._0;
    // if we're in the beginning of the epoch this might fail and will be updated next, so just use
    // default
    let next_operator =
        whitelist.getOperatorForNextEpoch().call().await.map(|res| res._0).unwrap_or_default();

    let lookahead = Arc::new(RwLock::new(Lookahead {
        curr,
        next: next_operator,
        updated_epoch: beacon_handle.current_epoch(),
    }));

    let lookahead_rw = lookahead.clone();

    spawn(async move {
        let mut last_log_slot = 0;

        let mut last_updated_epoch = 0;
        let mut last_updated_block = 0;
        let mut last_updated_slot = 0;

        loop {
            tokio::time::sleep(Duration::from_secs(beacon_handle.seconds_per_slot / 3)).await;

            let block_number = last_block_number.load(Ordering::Relaxed);
            let current_slot = beacon_handle.current_slot();
            let current_epoch = current_slot / beacon_handle.slots_per_epoch;
            let slot_in_epoch = current_slot % beacon_handle.slots_per_epoch;

            // only update once per epoch, late enough, and check there has been at least a block in
            // this epoch

            let last_lookahead = if last_updated_epoch != current_epoch &&
                slot_in_epoch > beacon_handle.slots_per_epoch / 2 &&
                block_number != last_updated_block
            {
                debug!(slot_in_epoch, current_slot, current_epoch, last_updated_epoch, block_number, "updating lookahead");
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
                                PreconfWhitelistErrors::InvalidOperatorCount(_) => {
                                    warn!("whitelist is empty (next)");
                                    Address::ZERO
                                }

                                _ => {
                                    let msg = format!("unhandled whitelist error (next): {err:?}");
                                    error!("{msg:?}");
                                    panic!("{msg}");
                                }
                            }
                        } else {
                            let msg = format!("failed to fetch next operator: {}", err);
                            error!("{msg:?}");
                            panic!("{msg}");
                        }
                    }
                };

                last_updated_epoch = current_epoch;
                last_updated_block = block_number;
                last_updated_slot = current_slot;

                {
                    let mut lookahead = lookahead_rw.write();
                    lookahead.updated_epoch = current_epoch;
                    lookahead.curr = current_operator;
                    lookahead.next = next_operator;
                    *lookahead
                }
            } else {
                *lookahead_rw.read()
            };

            if current_slot != last_log_slot {
                let (current_operator, next_operator) = if last_updated_epoch == 0 || current_epoch == last_updated_epoch {
                    (last_lookahead.curr, last_lookahead.next)
                } else {
                    (last_lookahead.next, Address::default())
                };

                info!(slot_in_epoch, current_slot, last_updated_slot, current_epoch, last_updated_epoch, %current_operator, %next_operator);
                last_log_slot = current_slot;
            }
        }
    }.in_current_span());

    Ok(LookaheadHandle::new(lookahead, preconfs_enabled_clone, config, beacon_handle))
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
    preconfs_enabled: Arc<AtomicBool>,
    checked: Instant,
    last: Lookahead,
    config: LookaheadConfig,
    pub beacon: BeaconHandle,
}

impl LookaheadHandle {
    fn new(
        lookahead: Arc<RwLock<Lookahead>>,
        preconfs_enabled: Arc<AtomicBool>,
        config: LookaheadConfig,
        beacon: BeaconHandle,
    ) -> Self {
        let last = *lookahead.read();
        Self { lookahead, preconfs_enabled, checked: Instant::now(), last, config, beacon }
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
        if !self.preconfs_enabled.load(Ordering::Relaxed) {
            return (false, "preconfs are disabled");
        }

        self.maybe_refresh();
        let lookahead = &self.last;
        let current_epoch = self.beacon.current_epoch();

        // current operator only sequences until here
        let cutoff_slot = self.beacon.slot_epoch_start(current_epoch) + self.beacon.slots_per_epoch -
            self.config.handover_window_slots;

        // next operator sequences after this time
        let cutoff_time = self.beacon.timestamp_of_slot(cutoff_slot) +
            self.config.handover_start_buffer_ms / 1_000;

        let (current_operator, next_operator) =
            if lookahead.updated_epoch == 0 || current_epoch == lookahead.updated_epoch {
                (lookahead.curr, lookahead.next)
            } else {
                (lookahead.next, Address::default())
            };

        match (operator == &current_operator, operator == &next_operator) {
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
        if !self.preconfs_enabled.load(Ordering::Relaxed) {
            return (false, "preconfs are disabled");
        }

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
        if !self.preconfs_enabled.load(Ordering::Relaxed) {
            return (false, "preconfs are disabled");
        }

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
