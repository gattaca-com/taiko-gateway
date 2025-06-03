use std::time::Duration;

use alloy_consensus::{
    BlobTransactionSidecar, SignableTransaction, Transaction, TxEip1559, TxEip4844,
    TxEip4844WithSidecar, TxEnvelope,
};
use alloy_eips::{eip7840::BlobParams, BlockNumberOrTag};
use alloy_network::TransactionBuilder;
use alloy_primitives::{Address, Bytes, B256, U256};
use alloy_provider::{Provider, ProviderBuilder, RootProvider};
use alloy_rpc_types::{BlockTransactionsKind, TransactionReceipt, TransactionRequest};
use alloy_rpc_types_txpool::TxpoolContentFrom;
use alloy_signer::SignerSync;
use alloy_signer_local::PrivateKeySigner;
use eyre::{ensure, eyre, OptionExt};
use pc_common::taiko::{
    pacaya::decode_propose_batch_with_expected_last_block_id_call, TaikoL1Client,
};
use serde_json::json;
use tracing::{error, info};
use url::Url;

use crate::manager::PendingProposal;

type AlloyProvider = RootProvider;

/// Wrapper around a L1 provider
pub struct L1Client {
    chain_id: u64,
    signer: PrivateKeySigner,
    taiko_client: TaikoL1Client,
    safe_lag: Duration,
    router_address: Address,
}

const _DEFAULT_GAS_LIMIT: u64 = 10_000_000;
const DEFAULT_MAX_FEE_PER_GAS: u128 = 10_000_000_000;
const DEFAULT_MAX_PRIORITY_FEE_PER_GAS: u128 = 1_000_000_000;
const DEFAULT_MAX_FEE_PER_BLOB_GAS: u128 = 5_000_000;
const BUFFER_PERCENTAGE: u128 = 140;

impl L1Client {
    pub async fn new(
        l1_rpc: Url,
        l1_contract: Address,
        signer: PrivateKeySigner,
        safe_lag: Duration,
        router_address: Address,
    ) -> eyre::Result<Self> {
        let provider = ProviderBuilder::new().disable_recommended_fillers().on_http(l1_rpc);
        let chain_id = provider.get_chain_id().await?;
        let taiko_client = TaikoL1Client::new(l1_contract, provider);

        Ok(Self { chain_id, signer, taiko_client, safe_lag, router_address })
    }

    pub fn provider(&self) -> &AlloyProvider {
        self.taiko_client.provider()
    }

    pub fn address(&self) -> Address {
        self.signer.address()
    }

    pub async fn last_meta_hash(&self) -> B256 {
        // let state = self.taiko_client.state().call().await?;

        // state.stats2.

        // let b = self.taiko_client.getBatch(1).call().await?;
        // b.batch_.metaHash

        // // let stats2 = state.stats2;
        // // let config = self.taiko_client.getConfig().call().await?._0;
        // // let last_batch = state.batches[(stats2.numBatches - 1) % config.batchRingBufferSize];
        // // last_batch.meta_hash

        // TODO: Zero picks the last batch, but we should pick the last on the state as a double
        // check

        B256::ZERO
    }

    #[allow(dead_code)]
    pub async fn get_nonce(&self) -> eyre::Result<u64> {
        self.provider()
            .get_transaction_count(self.signer.address())
            .await
            .map_err(|err| eyre!("failed to get nonce {err}"))
    }

    #[allow(dead_code)]
    pub async fn get_pending_nonce(&self) -> eyre::Result<u64> {
        let params = json!([self.signer.address(), "pending"]);

        let response: U256 = self
            .provider()
            .raw_request("eth_getTransactionCount".into(), params)
            .await
            .map_err(|err| eyre!("failed to get pending nonce {err}"))?;

        let nonce = response.try_into().unwrap();

        Ok(nonce)
    }

    pub async fn build_eip1559(
        &self,
        input: Bytes,
        bump_fees: bool,
        tip_cap: Option<u128>,
    ) -> eyre::Result<TxEnvelope> {
        let to = self.router_address;

        let tx = TransactionRequest::default()
            .with_to(to)
            .with_input(input.clone())
            .with_from(self.signer.address());

        let gas_limit = self
            .provider()
            .estimate_gas(&tx)
            .await
            .map_err(|err| eyre!(format!("failed gas estimation: {err}")))?;

        let (max_fee_per_gas, max_priority_fee_per_gas) =
            match self.provider().estimate_eip1559_fees(None).await {
                Ok(estimate) => (estimate.max_fee_per_gas, estimate.max_priority_fee_per_gas),
                Err(err) => {
                    error!(%err,"failed to estimate eip1559 fees");
                    (DEFAULT_MAX_FEE_PER_GAS, DEFAULT_MAX_PRIORITY_FEE_PER_GAS)
                }
            };

        // add buffer
        let gas_limit = (gas_limit as u128 * BUFFER_PERCENTAGE / 100) as u64;
        let mut max_fee_per_gas = max_fee_per_gas * BUFFER_PERCENTAGE / 100;
        let mut max_priority_fee_per_gas = max_priority_fee_per_gas * BUFFER_PERCENTAGE / 100;

        if let Some(tip_cap) = tip_cap {
            max_priority_fee_per_gas = tip_cap * BUFFER_PERCENTAGE / 100 + 1;
        }

        if bump_fees {
            max_fee_per_gas *= 2;
            max_priority_fee_per_gas *= 2;
        }

        let nonce = self.get_nonce().await?;
        let tx = TxEip1559 {
            chain_id: self.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            to: to.into(),
            input,
            value: Default::default(),
            access_list: Default::default(),
        };

        let sig = self.signer.sign_hash_sync(&tx.signature_hash())?;
        let signed = tx.into_signed(sig);

        Ok(signed.into())
    }

    pub async fn build_eip4844(
        &self,
        input: Bytes,
        sidecar: BlobTransactionSidecar,
        bump_fees: bool,
        tip_cap: Option<u128>,
        blob_fee_cap: Option<u128>,
    ) -> eyre::Result<TxEnvelope> {
        let to = self.router_address;

        let blob_versioned_hashes = sidecar.versioned_hashes().collect::<Vec<_>>();

        let mut tx = TransactionRequest::default()
            .with_to(to)
            .with_input(input.clone())
            .with_from(self.signer.address());
        tx.blob_versioned_hashes = Some(blob_versioned_hashes.clone());

        let gas_limit = self
            .provider()
            .estimate_gas(&tx)
            .await
            .map_err(|err| eyre!("failed gas estimation: {err}"))?;

        let (max_fee_per_gas, max_priority_fee_per_gas) =
            match self.provider().estimate_eip1559_fees(None).await {
                Ok(estimate) => (estimate.max_fee_per_gas, estimate.max_priority_fee_per_gas),
                Err(err) => {
                    error!(%err,"failed to estimate eip1559 fees");
                    (DEFAULT_MAX_FEE_PER_GAS, DEFAULT_MAX_PRIORITY_FEE_PER_GAS)
                }
            };

        let blob_gas_fee = self
            .provider()
            .get_block_by_number(BlockNumberOrTag::Latest, BlockTransactionsKind::Hashes)
            .await?
            .and_then(|block| block.header.next_block_blob_fee(BlobParams::prague()))
            .unwrap_or(DEFAULT_MAX_FEE_PER_BLOB_GAS);

        // add buffer
        let gas_limit = (gas_limit as u128 * BUFFER_PERCENTAGE / 100) as u64;
        let mut max_fee_per_gas = max_fee_per_gas * BUFFER_PERCENTAGE / 100;
        let mut max_priority_fee_per_gas = max_priority_fee_per_gas * BUFFER_PERCENTAGE / 100;
        let mut max_fee_per_blob_gas = blob_gas_fee * BUFFER_PERCENTAGE / 100;

        if let Some(tip_cap) = tip_cap {
            max_priority_fee_per_gas = tip_cap * BUFFER_PERCENTAGE / 100 + 1;
        }

        if let Some(blob_fee_cap) = blob_fee_cap {
            max_fee_per_blob_gas = blob_fee_cap * BUFFER_PERCENTAGE / 100 + 1;
        }

        if bump_fees {
            max_fee_per_gas *= 2;
            max_priority_fee_per_gas *= 2;
            max_fee_per_blob_gas *= 2;
        }

        let nonce = self.get_nonce().await?;
        let tx = TxEip4844 {
            chain_id: self.chain_id,
            nonce,
            gas_limit,
            max_fee_per_gas,
            max_priority_fee_per_gas,
            to,
            input,
            max_fee_per_blob_gas,
            blob_versioned_hashes,
            value: Default::default(),
            access_list: Default::default(),
        };

        let tx = TxEip4844WithSidecar::from_tx_and_sidecar(tx, sidecar);

        let sig = self.signer.sign_hash_sync(&tx.signature_hash())?;
        let signed = tx.into_signed(sig);

        Ok(signed.into())
    }

    pub async fn send_tx(&self, tx: TxEnvelope) -> eyre::Result<B256> {
        let pending = self.provider().send_tx_envelope(tx).await?;
        Ok(*pending.tx_hash())
    }

    pub async fn get_tx_receipt(&self, tx_hash: B256) -> eyre::Result<Option<TransactionReceipt>> {
        Ok(self.provider().get_transaction_receipt(tx_hash).await?)
    }

    pub async fn get_last_block_number(&self) -> eyre::Result<u64> {
        Ok(self.provider().get_block_number().await?)
    }

    /// Tx hash is for a tx that was just included in a block and we need to make sure it will not
    /// be re-orged out
    #[tracing::instrument(skip_all, name = "reorg_check", fields(tx_hash = %tx_hash))]
    pub async fn reorg_check(&self, tx_hash: B256) -> eyre::Result<()> {
        info!("starting reorg check");

        // after this time it's extremely unlikely that there's a re-org
        tokio::time::sleep(self.safe_lag).await;

        let tx_receipt = self
            .provider()
            .get_transaction_receipt(tx_hash)
            .await?
            .ok_or_eyre("tx not found, block was likely re-orged")?;

        ensure!(tx_receipt.block_number.is_some(), "tx was re-orged out");

        info!("check successful");

        Ok(())
    }

    pub async fn get_pending_proposals(&self) -> eyre::Result<Vec<PendingProposal>> {
        // fetch pending txpool
        let txpool: TxpoolContentFrom = self
            .provider()
            .raw_request("txpool_contentFrom".into(), [self.signer.address()])
            .await?;

        // decode proposals
        let mut pending = Vec::new();
        for tx in txpool.pending.into_values().chain(txpool.queued.into_values()) {
            if tx.to() == Some(self.router_address) {
                let (start_block_num, end_block_num, anchor_block_id) =
                    decode_propose_batch_with_expected_last_block_id_call(tx.input())?;
                pending.push(PendingProposal {
                    start_block_num,
                    end_block_num,
                    anchor_block_id,
                    nonce: tx.nonce(),
                    tx_hash: *tx.inner.tx_hash(),
                });
            }
        }

        Ok(pending)
    }
}

#[cfg(test)]
mod tests {
    use std::str::FromStr;

    use alloy_primitives::b256;
    use alloy_signer_local::LocalSigner;

    use super::*;

    #[ignore]
    #[tokio::test]
    async fn test_l1_client() {
        let l2_router_address =
            Address::from_str("0x1670100000000000000000000000000000010001").unwrap();
        let spammer_address =
            Address::from_str("0xF3384dCC14F03f079Ac7cd3C2299256B19261Bb0").unwrap();
        let l1_client = L1Client::new(
            Url::parse("https://rpc.helder-devnets.xyz").unwrap(),
            Address::ZERO,
            LocalSigner::random(),
            Duration::from_secs(10),
            l2_router_address,
        )
        .await
        .unwrap();
        let chain_id = l1_client.provider().get_chain_id().await.unwrap();
        println!("chain_id: {}", chain_id);

        // get nonce for spammer_address
        let nonce = l1_client.provider().get_transaction_count(spammer_address).await.unwrap();
        println!("nonce: {}", nonce);

        // get balance for spammer_address
        let balance = l1_client.provider().get_balance(spammer_address).await.unwrap();
        println!("balance: {}", balance);

        // get receipt for tx hash
        let tx_hash = b256!("0xd60dc1a0731e995b13aaf0c67a13b0578bce96a9fc125cee2affb83b0e6adce7");
        let receipt = l1_client.provider().get_transaction_receipt(tx_hash).await.unwrap();
        println!("receipt: {:?}", receipt);
    }
}
