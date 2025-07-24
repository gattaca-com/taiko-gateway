use std::time::Duration;

use alloy_consensus::constants::ETH_TO_WEI;
use alloy_primitives::{utils::format_ether, Address, B256, U256};
use alloy_provider::{
    fillers::{
        BlobGasFiller, ChainIdFiller, FillProvider, GasFiller, JoinFill, NonceFiller, WalletFiller,
    },
    network::EthereumWallet,
    Identity, Provider, ProviderBuilder, RootProvider,
};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::sol;
use eyre::bail;
use tracing::{debug, info, warn};
use url::Url;

use crate::{
    balance::ERC20::ERC20Instance,
    config::{GatewayConfig, TaikoChainParams},
    metrics::ProposerMetrics,
    taiko::pacaya::l1::TaikoL1::{self, TaikoL1Instance},
    utils::alert_discord,
};

sol!(
    #[derive(Debug, Eq, PartialEq)]
    #[sol(rpc)]
    #[allow(missing_docs)]
    ERC20,
    "../../abi/ERC20.abi.json"
);

// TODO: update alloy version we won't need this ugly type anymore
macro_rules! jf {
    ($jf:ty) => {
        $jf
    };
    ($jf1:ty, $($jf2:ty), +) => {
        JoinFill<$jf1, jf!($($jf2),+)>
    };
}
type RecommendedFillers = jf!(Identity, GasFiller, BlobGasFiller, NonceFiller, ChainIdFiller);
type FillersWithWallet = jf!(RecommendedFillers, WalletFiller<EthereumWallet>);
type RecommendedFillersType = FillProvider<FillersWithWallet, RootProvider>;
type TaikoL1InstanceType = TaikoL1Instance<(), RecommendedFillersType>;
type ERC20InstanceType = ERC20Instance<(), RecommendedFillersType>;

#[derive(Debug, Clone)]
pub struct BalanceManager {
    l1_contract: Address,
    operator: PrivateKeySigner, // signer of who will call proposeBatch
    taiko_config: TaikoChainParams,
    gateway_config: GatewayConfig,
    erc20: ERC20InstanceType,
    taiko_l1: TaikoL1InstanceType,
}

impl BalanceManager {
    pub fn new(
        taiko_token: Address,
        l1_contract: Address,
        l1_rpc: Url,
        operator: PrivateKeySigner,
        taiko_config: TaikoChainParams,
        gateway_config: &GatewayConfig,
    ) -> Self {
        let wallet = EthereumWallet::new(operator.clone());
        let l1_provider = ProviderBuilder::new().wallet(wallet).on_http(l1_rpc.clone());
        Self {
            l1_contract,
            operator,
            taiko_config,
            gateway_config: gateway_config.clone(),
            erc20: ERC20::new(taiko_token, l1_provider.clone()),
            taiko_l1: TaikoL1::new(l1_contract, l1_provider.clone()),
        }
    }

    pub async fn check_and_approve_balance(&self) -> eyre::Result<()> {
        let token_balance = self.get_token_balance().await?;
        let contract_balance = self.get_contract_balance().await?;
        let contract_allowance = self.get_allowance().await?;
        info!(
            token_balance = %format_ether(token_balance),
            contract_balance = %format_ether(contract_balance),
            contract_allowance = %format_ether(contract_allowance),
            "fetched taiko token info"
        );

        if contract_allowance < U256::MAX {
            self.approve_max_allowance().await?;
            let new_allowance = self.get_allowance().await?;
            info!(%new_allowance, "approved max allowance for taiko contract");
        }

        self.ensure_contract_balance(token_balance, contract_balance).await?;

        Ok(())
    }

    async fn ensure_contract_balance(
        &self,
        token_balance: U256,
        contract_balance: U256,
    ) -> eyre::Result<()> {
        let threshold = self.get_min_bond();

        if contract_balance >= threshold {
            debug!(
                contract_balance = format_ether(contract_balance),
                threshold = format_ether(threshold),
                "contract already has enough balance, no need to deposit"
            );
        } else {
            let mul_factor =
                U256::from((self.gateway_config.auto_deposit_bond_factor * 1000000.0).ceil());
            let target_amount = U256::from(f64::from(threshold * mul_factor)) / U256::from(1000000);

            let amount_to_deposit = target_amount.saturating_sub(contract_balance);
            if token_balance < amount_to_deposit {
                bail!(
                    "not enough balance to deposit, current: {}, required: {}",
                    token_balance,
                    amount_to_deposit
                );
            } else {
                let tx_hash = self.deposit_bond(amount_to_deposit).await?;
                info!(%tx_hash, "deposited bond");
            }
        }

        Ok(())
    }

    pub async fn get_token_balance(&self) -> eyre::Result<U256> {
        Ok(self.erc20.balanceOf(self.operator.address()).call().await?._0)
    }

    pub async fn approve_max_allowance(&self) -> eyre::Result<B256> {
        Ok(self.erc20.approve(self.l1_contract, U256::MAX).send().await?.watch().await?)
    }

    pub async fn get_allowance(&self) -> eyre::Result<U256> {
        Ok(self.erc20.allowance(self.operator.address(), self.l1_contract).call().await?._0)
    }

    pub async fn get_eth_balance(&self) -> eyre::Result<U256> {
        Ok(self.taiko_l1.provider().get_balance(self.operator.address()).await?)
    }

    pub async fn get_contract_balance(&self) -> eyre::Result<U256> {
        Ok(self.taiko_l1.bondBalanceOf(self.operator.address()).call().await?._0)
    }

    pub async fn deposit_bond(&self, amount: U256) -> eyre::Result<B256> {
        Ok(self.taiko_l1.depositBond(amount).send().await?.watch().await?)
    }

    pub fn get_min_bond(&self) -> U256 {
        let base = self.taiko_config.bond_base;
        // let per_batch = self.taiko_config.bond_per_block; // not used for now
        let n_batches_bond_threshold = U256::from(self.gateway_config.n_batches_bond_threshold);
        let threshold = base * n_batches_bond_threshold;

        info!(
            base = %format_ether(base),
            // per_batch = %format_ether(per_batch),
            n_batches_bond_threshold = %n_batches_bond_threshold,
            threshold = %format_ether(threshold),
            "calculated minimum bond amount"
        );

        threshold
    }

    pub async fn start_balance_monitor(self) {
        let _eth_thres = self.gateway_config.alert_eth_balance_threshold;
        let _token_thres = self.gateway_config.alert_total_token_threshold;
        let eth_balance_threshold = U256::from(_eth_thres * ETH_TO_WEI as f64);
        const TAIKO_TOKEN_UNITS: f64 = 1e18;
        let total_token_threshold = U256::from(_token_thres * TAIKO_TOKEN_UNITS);

        loop {
            tokio::time::sleep(Duration::from_secs(60)).await;

            // Fetch balances
            let (eth_balance_res, token_balance_res, contract_balance_res) = tokio::join!(
                self.get_eth_balance(),
                self.get_token_balance(),
                self.get_contract_balance()
            );

            let eth_balance = match eth_balance_res {
                Ok(balance) => {
                    ProposerMetrics::eth_balance(balance);
                    Some(balance)
                }
                Err(err) => {
                    warn!(%err, "failed to fetch ETH balance");
                    None
                }
            };

            let token_balance = match token_balance_res {
                Ok(balance) => {
                    ProposerMetrics::token_balance(balance);
                    Some(balance)
                }
                Err(err) => {
                    warn!(%err, "failed to fetch token balance");
                    None
                }
            };

            let contract_balance = match contract_balance_res {
                Ok(balance) => {
                    ProposerMetrics::token_bond(balance);
                    Some(balance)
                }
                Err(err) => {
                    warn!(%err, "failed to fetch token bond");
                    None
                }
            };

            // Auto deposit
            if self.gateway_config.auto_deposit_bond_enabled {
                if let (Some(token_balance), Some(contract_balance)) =
                    (token_balance, contract_balance)
                {
                    match self.ensure_contract_balance(token_balance, contract_balance).await {
                        Ok(_) => {}
                        Err(err) => warn!(%err, "failed to ensure contract balance"),
                    }
                }
            }

            // Discord Alerts
            if let Some(eth_balance) = eth_balance {
                self.alert_balance("ETH Balance", eth_balance, eth_balance_threshold);
            }

            if let (Some(token_balance), Some(contract_balance)) = (token_balance, contract_balance)
            {
                let total = token_balance + contract_balance;
                self.alert_balance("TAIKO Token", total, total_token_threshold);
            }
        }
    }

    pub fn alert_balance(&self, label: &str, balance: U256, threshold: U256) {
        if balance < threshold {
            let msg = format!("{} balance is below threshold: {} < {}", label, format_ether(balance), format_ether(threshold));
            warn!("{}", msg);
            alert_discord(&msg);
        }
    }
}
