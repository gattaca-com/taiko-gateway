use alloy_primitives::{Address, U256};
use alloy_provider::{network::EthereumWallet, ProviderBuilder};
use alloy_signer_local::PrivateKeySigner;
use alloy_sol_types::sol;
use tracing::info;
use url::Url;

use crate::taiko::pacaya::l1::TaikoL1;

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
    signer: PrivateKeySigner, // signer of who will call proposeBatch
) -> eyre::Result<()> {
    let signer_address = signer.address();
    let wallet = EthereumWallet::new(signer);

    let l1_provider = ProviderBuilder::new().wallet(wallet).on_http(l1_rpc);

    let erc20 = ERC20::new(taiko_token, l1_provider.clone());
    let taiko_l1 = TaikoL1::new(l1_contract, l1_provider);

    let current_balance = erc20.balanceOf(signer_address).call().await?._0;
    let contract_balance = taiko_l1.bondBalanceOf(signer_address).call().await?._0;
    let contract_allowance = erc20.allowance(signer_address, l1_contract).call().await?._0;

    info!(%current_balance, %contract_balance, %contract_allowance, "fetched taiko token info");

    if contract_allowance < U256::MAX {
        info!("approving max allowance for Taiko token to TaikoInbox contract");
        let tx_hash = erc20.approve(l1_contract, U256::MAX).send().await?.watch().await?;
        let new_allowance = erc20.allowance(signer_address, l1_contract).call().await?._0;
        info!(%tx_hash, %new_allowance, "increased allowance");
    }

    if current_balance > U256::ZERO {
        info!("depositing bond to TaikoInbox contract");
        let tx_hash = taiko_l1.depositBond(current_balance).send().await?.watch().await?;
        info!(%tx_hash, "deposited bond");
    }

    Ok(())
}
