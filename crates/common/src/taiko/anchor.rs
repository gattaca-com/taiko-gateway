use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_primitives::{B256, U256};
use alloy_provider::ProviderBuilder;
use alloy_sol_types::SolCall;

use super::{
    fixed_signer::sign_fixed_k,
    l2::TaikoL2::{self, anchorV2Call},
    ANCHOR_GAS_LIMIT, GOLDEN_TOUCH_SIGNER,
};
use crate::config::{TaikoChainConfig, TaikoConfig};

/// Mock some of the logic in `anchorV2` to compute the correct base fee
/// IMPORTANT: some calls are to a stateful contract so it matters whether the RPC has preconf state
/// or not
// TODO: use a stateful provider to avoid recreating provider/taiko_l2 each time
pub async fn compute_next_base_fee(
    config: &TaikoConfig,
    chain_config: &TaikoChainConfig,
    parent_gas_used: u64,
    parent_timestamp: u64,
    anchor_timestamp: u64,
) -> eyre::Result<u128> {
    assert!(anchor_timestamp >= parent_timestamp);
    // This can be zero (eg in propose multiple blocks)
    let timestamp_elapsed = anchor_timestamp - parent_timestamp;

    let l2_provider = ProviderBuilder::new().on_http(config.preconf_url.clone());
    let taiko_l2 = TaikoL2::new(config.l2_contract_address, l2_provider);

    let parent_gas_target = taiko_l2.parentGasTarget().call().await?._0;
    let mut parent_gas_excess = taiko_l2.parentGasExcess().call().await?._0;

    // https://github.com/taikoxyz/taiko-mono/blob/ontake_preconfs/packages/protocol/contracts/L2/TaikoL2.sol#L144-L155
    let new_gas_target = chain_config.base_fee_config.gasIssuancePerSecond as u64 *
        chain_config.base_fee_config.adjustmentQuotient as u64;
    if parent_gas_target != new_gas_target && parent_gas_target != 0 {
        parent_gas_excess = taiko_l2
            .adjustExcess(parent_gas_excess, parent_gas_target, new_gas_target)
            .call()
            .await?
            .newGasExcess_;
    }

    // https://github.com/taikoxyz/taiko-mono/blob/ontake_preconfs/packages/protocol/contracts/L2/TaikoL2.sol#L157-L164
    let base_fee = taiko_l2
        .calculateBaseFee(
            chain_config.base_fee_config.clone(),
            timestamp_elapsed,
            parent_gas_excess,
            parent_gas_used.try_into()?,
        )
        .call()
        .await?;

    Ok(base_fee.basefee_.try_into()?)
}

/// Assemble the AnchorV2 transaction and sign it with the golden touch private key
// https://github.com/taikoxyz/taiko-geth/blob/preconfs/consensus/taiko/consensus.go#L284
pub fn assemble_anchor_v2(
    config: &TaikoConfig,
    chain_config: &TaikoChainConfig,
    anchor_block_id: u64,
    anchor_state_root: B256,
    parent_gas_used: u32,
    l2_parent_block_number: u64,
    l2_base_fee: u128, // base fee where this tx will be included
) -> TxEnvelope {
    let input = anchorV2Call::new((
        anchor_block_id,
        anchor_state_root,
        parent_gas_used,
        chain_config.base_fee_config.clone(),
    ))
    .abi_encode();

    let tx = TxEip1559 {
        chain_id: config.chain_id,
        nonce: l2_parent_block_number, // one anchor tx per L2 block -> block number = nonce
        gas_limit: ANCHOR_GAS_LIMIT,
        max_fee_per_gas: l2_base_fee,
        max_priority_fee_per_gas: 0,
        to: config.l2_contract_address.into(),
        value: U256::ZERO,
        access_list: Default::default(),
        input: input.into(),
    };

    let signature = sign_fixed_k(tx.signature_hash(), GOLDEN_TOUCH_SIGNER.as_nonzero_scalar())
        .expect("failed golden touch signature");
    let signed = tx.into_signed(signature);

    signed.into()
}

#[cfg(test)]
mod tests {
    use alloy_primitives::{address, b256, Bytes};
    use alloy_sol_types::SolCall;
    use TaikoL2::anchorV2Call;

    use super::*;
    use crate::config::TaikoChainConfig;

    #[test]
    fn test_input() {
        let anchor_block_id = 1;
        let anchor_state_root =
            b256!("1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef");
        let parent_gas_used = 100;
        let base_fee_config = TaikoChainConfig::new_helder().base_fee_config;

        let l2_provider = ProviderBuilder::new().on_http("http://abc.xyz".parse().unwrap());
        let taiko_l2 = address!("1234567890abcdef1234567890abcdef12345678");

        let taiko_l2 = TaikoL2::new(taiko_l2, l2_provider);

        let input = taiko_l2
            .anchorV2(anchor_block_id, anchor_state_root, parent_gas_used, base_fee_config.clone())
            .calldata()
            .clone();

        let input_check = anchorV2Call::new((
            anchor_block_id,
            anchor_state_root,
            parent_gas_used,
            base_fee_config,
        ))
        .abi_encode();

        assert_eq!(input, Bytes::from(input_check));
    }
}
