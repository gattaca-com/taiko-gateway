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
    let next_operator = whitelist.getOperatorForNextEpoch().call().await?._0;

    let lookahead =
        Arc::new(RwLock::new(Lookahead { curr: current_operator, next: next_operator }));

    let lookahead_rw = lookahead.clone();

    let err_operator_not_available_yet = alloy_primitives::hex::encode(
        PreconfWhitelistErrors::OperatorNotAvailableYet(OperatorNotAvailableYet {}).abi_encode(),
    );

    spawn(async move {
        loop {
            match fetch_operators(&whitelist).await {
                Ok((current_operator, next_operator)) => {
                    let current_slot = beacon_handle.current_slot();
                    let current_epoch = beacon_handle.current_epoch();
                    let remaining_slots =
                        beacon_handle.slots_per_epoch - beacon_handle.slot_in_epoch();

                    info!(current_slot, remaining_slots, current_epoch, %current_operator, %next_operator, "fetched operators");

                    {
                        let mut lookahead = lookahead_rw.write();
                        lookahead.curr = current_operator;
                        lookahead.next = next_operator;
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

#[derive(Debug, Clone, Copy)]
pub struct Lookahead {
    pub curr: Address,
    pub next: Address,
}

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
        if self.checked.elapsed() > Duration::from_secs(2) {
            self.last = *self.lookahead.read();
            self.checked = Instant::now();
        }
    }

    pub fn can_sequence(&mut self, operator: &Address) -> (bool, &str) {
        self.maybe_refresh();
        let lookahead = &self.last;

        if &lookahead.curr == operator && &lookahead.next == operator {
            return (true, "operator is the same for both current and next");
        }

        // either current operator before delay slots
        let current_check = &lookahead.curr == operator &&
            (self.beacon.slot_in_epoch() <
                self.beacon.slots_per_epoch - self.config.delay_slots);

        if current_check {
            return (true, "current operator before delay slots");
        }

        // or next operator after buffer_secs
        let cutoff_slot =
            self.beacon.slot_epoch_start() + self.beacon.slots_per_epoch - self.config.delay_slots;
        let cutoff_time = self.beacon.timestamp_of_slot(cutoff_slot) + self.config.buffer_secs;

        let next_check = &lookahead.next == operator && (utcnow_sec() > cutoff_time);

        if next_check {
            return (true, "next operator after buffer_secs");
        }

        (false, "operator is not the current or next operator")
    }

    // Clear all remaining batches if we're approaching a different operator turn
    pub fn should_clear_proposal(&mut self, operator: &Address) -> bool {
        self.maybe_refresh();
        let lookahead = &self.last;

        // current operator after delay slots
        let current_check = &lookahead.curr == operator &&
            (self.beacon.slot_in_epoch() >=
                self.beacon.slots_per_epoch - self.config.delay_slots);

        current_check && &lookahead.next != operator
    }
}
