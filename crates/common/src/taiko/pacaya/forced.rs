use std::time::Duration;

use alloy_eips::eip4844::BlobTransactionSidecarItem;
use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder, RootProvider};
use eyre::{eyre, OptionExt};
use serde::Deserialize;
use tracing::debug;
use url::Url;

use super::{decode_tx_list, forced_inclusion::IForcedInclusionStore::ForcedInclusion};
use crate::{
    beacon::BeaconHandle,
    sequencer::Order,
    taiko::{blob::decode_blob_data, ForcedInclusionStore, TaikoWrapper},
};

#[derive(Deserialize)]
struct BlobResponse {
    data: Vec<BlobTransactionSidecarItem>,
}

#[derive(Debug, Clone)]
pub struct ForcedInclusionInfo {
    pub inclusion: ForcedInclusion,
    pub min_txs_per_forced: usize,
}

pub struct ForcedInclusionClient {
    l1_provider: RootProvider,
    beacon_url: Url,
    beacon_handle: BeaconHandle,
    inclusion_store: ForcedInclusionStore,
    taiko_wrapper: TaikoWrapper,
}

impl ForcedInclusionClient {
    pub fn new(
        l1_rpc: Url,
        beacon_url: Url,
        beacon_handle: BeaconHandle,
        taiko_wrapper_address: Address,
        inclusion_store_address: Address,
    ) -> Self {
        let provider = ProviderBuilder::new().disable_recommended_fillers().on_http(l1_rpc);
        let inclusion_store = ForcedInclusionStore::new(inclusion_store_address, provider.clone());
        let taiko_wrapper = TaikoWrapper::new(taiko_wrapper_address, provider.clone());

        Self { l1_provider: provider, beacon_url, beacon_handle, inclusion_store, taiko_wrapper }
    }

    #[tracing::instrument(skip_all, name = "forced_inclusion")]
    pub async fn get_forced_txs(&self) -> eyre::Result<Option<(ForcedInclusionInfo, Vec<Order>)>> {
        let head = self.inclusion_store.head().call().await?._0;
        let tail = self.inclusion_store.tail().call().await?._0;

        if head >= tail {
            return Ok(None);
        }

        debug!(head, tail, "fetching forced tx list");
        let inclusion = self.inclusion_store.getForcedInclusion(U256::from(head)).call().await?._0;
        debug!(?inclusion, "found inclusion");

        if inclusion.createdAtBatchId == 0 {
            return Ok(None);
        }

        let bn = inclusion.blobCreatedIn;
        let block = self
            .l1_provider
            .get_block_by_number(bn.into(), false.into())
            .await?
            .ok_or_eyre("missing block")?;
        let slot = (block.header.timestamp - self.beacon_handle.genesis_time_sec) /
            self.beacon_handle.seconds_per_slot;
        let client = reqwest::ClientBuilder::new().timeout(Duration::from_secs(5)).build()?;
        let blob_url = self.beacon_url.join(&format!("eth/v1/beacon/blob_sidecars/{slot}"))?;

        let blobs = client.get(blob_url).send().await?.json::<BlobResponse>().await?.data;
        let blob = blobs
            .into_iter()
            .find(|b| b.to_kzg_versioned_hash() == inclusion.blobHash.0)
            .ok_or_eyre(eyre!("missing blob with hash: {}", inclusion.blobHash))?;

        let blob_start = inclusion.blobByteOffset as usize;
        let blob_end = blob_start + inclusion.blobByteSize as usize;
        debug!(blob_index = blob.index, blob_start, blob_end, "decoding txs in blob");

        let bytes = decode_blob_data(blob.blob.as_slice());

        let encoded =
            bytes.as_slice().get(blob_start..blob_end).ok_or_eyre("missing slice in blob")?;

        let txs = decode_tx_list(encoded)?;

        let min_txs_per_forced =
            self.taiko_wrapper.MIN_TXS_PER_FORCED_INCLUSION().call().await?._0 as usize;

        let txs = txs.into_iter().map(Order::new).collect::<eyre::Result<Vec<Order>>>()?;
        debug!(min_txs_per_forced, txs = txs.len(), "got txs in forced inclusion");

        Ok(Some((ForcedInclusionInfo { inclusion, min_txs_per_forced }, txs)))
    }
}
