use alloy_consensus::TxEnvelope;
use alloy_eips::BlockNumberOrTag;
use alloy_network::{Ethereum, TransactionBuilder, TransactionBuilder4844};
use alloy_provider::{
    fillers::{FillProvider, JoinFill, WalletFiller},
    network::EthereumWallet,
    Provider, ProviderBuilder, WalletProvider,
};
use alloy_rpc_types::{BlockTransactionsKind, TransactionReceipt, TransactionRequest};
use alloy_signer_local::PrivateKeySigner;
use alloy_transport_http::Http;
use eyre::{bail, eyre};
use reqwest::Client;
use tracing::error;
use url::Url;

use crate::types::{BlobFields, CalldataFields};

type AlloyProvider = FillProvider<
    JoinFill<
        JoinFill<
            alloy_provider::Identity,
            JoinFill<
                alloy_provider::fillers::GasFiller,
                JoinFill<
                    alloy_provider::fillers::BlobGasFiller,
                    JoinFill<
                        alloy_provider::fillers::NonceFiller,
                        alloy_provider::fillers::ChainIdFiller,
                    >,
                >,
            >,
        >,
        WalletFiller<EthereumWallet>,
    >,
    alloy_provider::RootProvider<Http<Client>>,
    Http<Client>,
    Ethereum,
>;

/// Handles inclusion of L1 transactions
// TODO: leverage L1 inclusion preconfs
pub struct L1Includer {
    provider: AlloyProvider,
}

impl L1Includer {
    pub fn new(l1_rpc: Url, signer: PrivateKeySigner) -> Self {
        let wallet = EthereumWallet::from(signer);

        let provider =
            ProviderBuilder::new().with_recommended_fillers().wallet(wallet).on_http(l1_rpc);

        Self { provider }
    }

    pub fn provider(&self) -> &AlloyProvider {
        &self.provider
    }

    pub async fn generate_eip1559_tx(
        &self,
        CalldataFields { to, input }: CalldataFields,
    ) -> eyre::Result<TxEnvelope> {
        let tx = TransactionRequest::default()
            .with_to(to)
            .with_input(input)
            .with_from(self.provider.default_signer_address());

        let gas_limit = 10_000_000;
        let (max_fee_per_gas, max_priority_fee_per_gas) =
            match self.provider.estimate_eip1559_fees(None).await {
                Ok(estimate) => (estimate.max_fee_per_gas, estimate.max_priority_fee_per_gas),
                Err(err) => {
                    error!(%err,"failed to estimate eip1559 fees");
                    (10_000_000_000, 1_000_000_000)
                }
            };

        // add 20% buffer
        let max_fee_per_gas = max_fee_per_gas * 120 / 100;
        let max_priority_fee_per_gas = max_priority_fee_per_gas * 120 / 100;

        let tx = tx
            .with_gas_limit(gas_limit)
            .with_max_fee_per_gas(max_fee_per_gas)
            .with_max_priority_fee_per_gas(max_priority_fee_per_gas);

        match self.provider.fill(tx).await {
            Ok(tx) => {
                let tx = tx.as_envelope().unwrap().clone();
                Ok(tx)
            }
            Err(err) => {
                bail!("failed to fill eip559 tx err={err}")
            }
        }
    }

    pub async fn generate_blob_tx(
        &self,
        BlobFields { to, input, sidecar }: BlobFields,
    ) -> eyre::Result<TxEnvelope> {
        let tx = TransactionRequest::default().with_to(to).with_input(input);

        let gas_limit = self.provider.estimate_gas(&tx).await?;
        let estimate = self.provider.estimate_eip1559_fees(None).await?;
        let max_fee_per_gas = estimate.max_fee_per_gas;
        let max_priority_fee_per_gas = estimate.max_priority_fee_per_gas;
        let blob_gas_fee = self
            .provider
            .get_block_by_number(BlockNumberOrTag::Latest, BlockTransactionsKind::Hashes)
            .await?
            .and_then(|block| block.header.next_block_blob_fee())
            .unwrap_or(5_000_000);

        // add 20% buffer
        let gas_limit = 1_000_000.max(gas_limit * 120 / 100);
        let max_fee_per_gas = max_fee_per_gas * 120 / 100;
        let max_priority_fee_per_gas = max_priority_fee_per_gas * 120 / 100;
        let max_fee_per_blob_gas = blob_gas_fee * 120 / 100;

        let tx = tx
            .with_gas_limit(gas_limit)
            .with_max_fee_per_gas(max_fee_per_gas)
            .with_max_priority_fee_per_gas(max_priority_fee_per_gas)
            .with_max_fee_per_blob_gas(max_fee_per_blob_gas)
            .with_blob_sidecar(sidecar);

        match self.provider.fill(tx).await {
            Ok(tx) => Ok(tx.as_envelope().ok_or(eyre!("not an envelope"))?.clone()),
            Err(err) => {
                bail!("failed to fill blob tx {err}")
            }
        }
    }

    pub async fn send_tx(&self, tx: TxEnvelope) -> eyre::Result<TransactionReceipt> {
        let pending = self.provider.send_tx_envelope(tx).await?;
        let receipt = pending.get_receipt().await?;
        Ok(receipt)
    }
}
