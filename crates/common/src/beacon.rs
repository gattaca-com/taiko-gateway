use std::time::Duration;

use eyre::ensure;
use serde::Deserialize;
use url::Url;

use crate::utils::{quoted_u64, utcnow_sec};

#[derive(Debug, Clone, Copy)]
pub struct BeaconHandle {
    pub slots_per_epoch: u64,
    pub genesis_time_sec: u64,
    pub seconds_per_slot: u64,
}

impl BeaconHandle {
    pub fn new(slots_per_epoch: u64, genesis_time_sec: u64, seconds_per_slot: u64) -> Self {
        Self { slots_per_epoch, genesis_time_sec, seconds_per_slot }
    }

    /// Calculates the current slot purely based on the current timestamp
    pub fn current_slot(&self) -> u64 {
        (utcnow_sec() - self.genesis_time_sec) / self.seconds_per_slot
    }

    /// Calculates the current epoch purely based on the current timestamp
    pub fn current_epoch(&self) -> u64 {
        self.current_slot() / self.slots_per_epoch
    }

    /// Number of slots into the current epoch (0-slots_per_epoch)
    pub fn slot_in_epoch(&self) -> u64 {
        self.current_slot() % self.slots_per_epoch
    }

    /// First slot in the current epoch
    pub fn slot_epoch_start(&self, epoch: u64) -> u64 {
        epoch * self.slots_per_epoch
    }

    pub fn timestamp_of_slot(&self, slot: u64) -> u64 {
        self.genesis_time_sec + slot * self.seconds_per_slot
    }
}

pub async fn init_beacon(beacon_url: Url) -> eyre::Result<BeaconHandle> {
    let client = reqwest::ClientBuilder::new().timeout(Duration::from_secs(5)).build()?;

    // chain specs
    let spec_url = beacon_url.join("eth/v1/config/spec")?;
    let spec = client.get(spec_url).send().await?.json::<BeaconApiResponse<Spec>>().await?.data;

    // genesis
    let genesis_url = beacon_url.join("eth/v1/beacon/genesis")?;
    let genesis =
        client.get(genesis_url).send().await?.json::<BeaconApiResponse<Genesis>>().await?.data;

    let genesis_time_sec = genesis.genesis_time;

    let beacon_handle =
        BeaconHandle::new(spec.slots_per_epoch, genesis_time_sec, spec.seconds_per_slot);

    // fetch current slot
    let headers_url = beacon_url.join("eth/v1/beacon/headers/head")?;
    let header =
        client.get(headers_url).send().await?.json::<BeaconApiResponse<Header>>().await?.data;
    let head_slot = header.header.message.slot;

    // head slot is within 2 of beacon handle slot
    ensure!(
        (head_slot as i64 - beacon_handle.current_slot() as i64).abs() <= 2,
        "head slot is too far from current slot, head_slot={head_slot}, current_slot={}",
        beacon_handle.current_slot()
    );

    Ok(beacon_handle)
}

#[derive(Deserialize)]
struct BeaconApiResponse<T> {
    data: T,
}

#[derive(Deserialize)]
struct Genesis {
    #[serde(with = "quoted_u64")]
    genesis_time: u64,
}

#[derive(Deserialize)]
#[serde(rename_all = "UPPERCASE")]
struct Spec {
    #[serde(with = "quoted_u64")]
    seconds_per_slot: u64,
    #[serde(with = "quoted_u64")]
    slots_per_epoch: u64,
}

#[derive(Deserialize)]
struct Header {
    header: HeaderInner,
}

#[derive(Deserialize)]
struct HeaderInner {
    message: Message,
}

#[derive(Deserialize)]
struct Message {
    #[serde(with = "quoted_u64")]
    slot: u64,
}
