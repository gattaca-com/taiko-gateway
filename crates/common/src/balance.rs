use std::time::Duration;

use alloy_primitives::{Address, U256};
use alloy_provider::{network::EthereumWallet, Provider, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::sol;
use eyre::bail;
use tracing::{info, warn};
use url::Url;

use crate::{
    config::TaikoChainParams, metrics::ProposerMetrics, runtime::spawn, taiko::pacaya::l1::TaikoL1,
};

sol!(
    #[derive(Debug, Eq, PartialEq)]
    #[sol(rpc)]
    #[allow(missing_docs)]
    ERC20,
    "../../abi/ERC20.abi.json"
);

// TODO: this should allow more flexibility for the caller
pub async fn check_and_approve_balance(
    taiko_token: Address,
    l1_contract: Address,
    l1_rpc: Url,
    operator: PrivateKeySigner, // signer of who will call proposeBatch
    taiko_config: &TaikoChainParams,
) -> eyre::Result<()> {
    let operator_address = operator.address();
    let wallet = EthereumWallet::new(operator);

    let l1_provider = ProviderBuilder::new().wallet(wallet).on_http(l1_rpc.clone());

    let erc20 = ERC20::new(taiko_token, l1_provider.clone());
    let taiko_l1 = TaikoL1::new(l1_contract, l1_provider);

    let current_balance = erc20.balanceOf(operator_address).call().await?._0;
    let contract_balance = taiko_l1.bondBalanceOf(operator_address).call().await?._0;
    let contract_allowance = erc20.allowance(operator_address, l1_contract).call().await?._0;

    info!(%current_balance, %contract_balance, %contract_allowance, "fetched taiko token info");

    if contract_allowance < U256::MAX {
        info!("approving max allowance for Taiko token to TaikoInbox contract");
        let tx_hash = erc20.approve(l1_contract, U256::MAX).send().await?.watch().await?;
        let new_allowance = erc20.allowance(operator_address, l1_contract).call().await?._0;
        info!(%tx_hash, %new_allowance, "increased allowance");
    }

    if current_balance > U256::ZERO {
        info!("depositing bond to TaikoInbox contract");
        let tx_hash = taiko_l1.depositBond(current_balance).send().await?.watch().await?;
        info!(%tx_hash, "deposited bond");
    }

    let current_balance = erc20.balanceOf(operator_address).call().await?._0;
    let contract_balance = taiko_l1.bondBalanceOf(operator_address).call().await?._0;

    let total_balance = current_balance + contract_balance;
    // 1 block every 2 seconds for 32 L1 blocks
    let min_bond = U256::from(taiko_config.bond_base) +
        U256::from(taiko_config.bond_per_block) * U256::from(200);

    if total_balance < min_bond {
        bail!("total balance is too low, get more tokens: {} < {}", total_balance, min_bond);
    }

    spawn(record_balances_loop(taiko_token, l1_contract, l1_rpc, operator_address));

    Ok(())
}

async fn record_balances_loop(
    taiko_token: Address,
    l1_contract: Address,
    l1_rpc: Url,
    operator_address: Address,
) {
    let l1_provider = ProviderBuilder::new().disable_recommended_fillers().on_http(l1_rpc);
    let erc20 = ERC20::new(taiko_token, l1_provider.clone());
    let taiko_l1 = TaikoL1::new(l1_contract, l1_provider);

    loop {
        tokio::time::sleep(Duration::from_secs(60)).await;

        match taiko_l1.provider().get_balance(operator_address).await {
            Ok(balance) => {
                ProposerMetrics::eth_balance(balance);
            }

            Err(err) => {
                warn!(%err, "failed to fetch ETH balance");
            }
        }

        match erc20.balanceOf(operator_address).call().await {
            Ok(balance) => {
                ProposerMetrics::token_balance(balance._0);
            }

            Err(err) => {
                warn!(%err, "failed to fetch token balance");
            }
        };

        match taiko_l1.bondBalanceOf(operator_address).call().await {
            Ok(balance) => {
                ProposerMetrics::token_bond(balance._0);
            }

            Err(err) => {
                warn!(%err, "failed to fetch token bond");
            }
        };
    }
}
