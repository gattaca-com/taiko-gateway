use alloy_provider::{Provider, ProviderBuilder};

use crate::{
    config::{L1ChainConfig, L2ChainConfig, TaikoChainParams},
    taiko::{
        pacaya::{l1::TaikoL1, l2::TaikoL2},
        GOLDEN_TOUCH_ADDRESS,
    },
};

/// Run checks to ensure that we have all the correct config and values, returns the loaded chain
/// params
pub async fn get_and_validate_config(
    l1_config: L1ChainConfig,
    l2_config: L2ChainConfig,
) -> eyre::Result<TaikoChainParams> {
    let l1_provider = ProviderBuilder::new().on_http(l1_config.rpc_url);
    let taiko_l1 = TaikoL1::new(l2_config.l1_contract, l1_provider);

    let l2_provider = ProviderBuilder::new().on_http(l2_config.rpc_url.clone());
    let taiko_l2 = TaikoL2::new(l2_config.l2_contract, l2_provider);

    // L1 data
    let chain_id = taiko_l1.provider().get_chain_id().await?;
    assert_eq!(chain_id, l1_config.chain_id, "l1 chain id");

    let taiko_config = taiko_l1.getConfig().call().await?._0;
    assert_eq!(taiko_config.chainId, l2_config.chain_id, "l2 chain id in l1 contract");

    // L2 data
    let chain_id = taiko_l2.provider().get_chain_id().await?;
    assert_eq!(chain_id, l2_config.chain_id, "l2 chain id");

    let golden_touch = taiko_l2.GOLDEN_TOUCH_ADDRESS().call().await?._0;
    assert_eq!(golden_touch, GOLDEN_TOUCH_ADDRESS, "golden touch address");

    let chain_id = taiko_l2.l1ChainId().call().await?._0;
    assert_eq!(chain_id, l1_config.chain_id, "l1 chain id in l2 contract");

    let chain_config = TaikoChainParams::new(
        taiko_config.baseFeeConfig.into(),
        taiko_config.maxAnchorHeightOffset,
        taiko_config.blockMaxGasLimit as u64,
    );

    Ok(chain_config)
}
