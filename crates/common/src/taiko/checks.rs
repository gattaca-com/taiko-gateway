use std::time::Duration;

use alloy_primitives::{Address, U256};
use alloy_provider::{Provider, ProviderBuilder};
use tracing::{info, warn};

use crate::{
    config::{L1ChainConfig, L2ChainConfig, TaikoChainParams},
    metrics::record_info,
    taiko::{
        pacaya::{l1::TaikoL1, l2::TaikoL2, preconf::PreconfWhitelist},
        GOLDEN_TOUCH_ADDRESS,
    },
};

/// Run checks to ensure that we have all the correct config and values, returns the loaded chain
/// params
pub async fn get_and_validate_config(
    l1_config: L1ChainConfig,
    l2_config: L2ChainConfig,
    operator: Address,
    coinbase: Address,
) -> eyre::Result<TaikoChainParams> {
    let l1_provider = ProviderBuilder::new().on_http(l1_config.rpc_url);
    let taiko_l1 = TaikoL1::new(l2_config.l1_contract, l1_provider.clone());
    let whitelist = PreconfWhitelist::new(l2_config.whitelist_contract, l1_provider);

    let l2_provider = ProviderBuilder::new().on_http(l2_config.rpc_url.clone());
    let taiko_l2 = TaikoL2::new(l2_config.l2_contract, l2_provider);

    let l1_chain_id = taiko_l1.provider().get_chain_id().await?;
    let l2_chain_id = taiko_l2.provider().get_chain_id().await?;

    record_info(l1_chain_id, l2_chain_id, operator, coinbase, l2_config);

    // L1 data
    let taiko_config = taiko_l1.pacayaConfig().call().await?._0;
    assert_eq!(taiko_config.chainId, l2_chain_id, "l2 chain id in l1 contract");

    // L2 data
    let golden_touch = taiko_l2.GOLDEN_TOUCH_ADDRESS().call().await?._0;
    assert_eq!(golden_touch, GOLDEN_TOUCH_ADDRESS, "golden touch address");

    let chain_id = taiko_l2.l1ChainId().call().await?._0;
    assert_eq!(chain_id, l1_chain_id, "l1 chain id in l2 contract");

    // fetch forever till we are in the whitelist
    loop {
        let operator_count: usize = whitelist.operatorCount().call().await?._0.try_into()?;
        let mut operators = Vec::with_capacity(operator_count);

        for i in 0..operator_count {
            let operator = whitelist.operatorIndexToOperator(U256::from(i)).call().await?.operator;
            operators.push(operator);
        }

        if let Some(this_index) = operators.iter().position(|&op| op == operator) {
            info!(this_index, ?operators, "fetched operators");
            break;
        } else {
            warn!(operator= %operator, whitelist=? operators, "provided address is not in whitelist! sleeping for 60s");
        };

        tokio::time::sleep(Duration::from_secs(60)).await;
    }

    let chain_config = TaikoChainParams::new(
        l2_chain_id,
        taiko_config.baseFeeConfig.into(),
        taiko_config.maxAnchorHeightOffset,
        taiko_config.blockMaxGasLimit as u64,
    );

    Ok(chain_config)
}
