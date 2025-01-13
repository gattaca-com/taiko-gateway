use alloy_consensus::{SignableTransaction, TxEnvelope, TypedTransaction};
use alloy_network::TransactionBuilder;
use alloy_primitives::{B256, U256};
use alloy_provider::ProviderBuilder;
use alloy_signer_local::PrivateKeySigner;
use eyre::eyre;

use super::{
    fixed_signer::sign_fixed_k, l2::TaikoL2, ANCHOR_GAS_LIMIT, GOLDEN_TOUCH_ADDRESS,
    GOLDEN_TOUCH_PRIVATE_KEY,
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
) -> eyre::Result<TxEnvelope> {
    let l2_provider = ProviderBuilder::new().on_http("http://never-call-me.xyz".parse().unwrap());

    let taiko_l2 = TaikoL2::new(config.l2_contract_address, l2_provider);

    let tx_request = taiko_l2
        .anchorV2(
            anchor_block_id,
            anchor_state_root,
            parent_gas_used,
            chain_config.base_fee_config.clone(),
        )
        .into_transaction_request()
        .with_chain_id(config.chain_id)
        .with_nonce(l2_parent_block_number) // one anchor tx per L2 block -> block number = nonce
        .with_to(config.l2_contract_address)
        .with_value(U256::ZERO)
        .with_gas_limit(ANCHOR_GAS_LIMIT)
        .with_max_fee_per_gas(l2_base_fee)
        .with_max_priority_fee_per_gas(0)
        .with_from(GOLDEN_TOUCH_ADDRESS);

    tx_request.complete_1559().map_err(|err| eyre!("missing 1559 keys {err:?}"))?;

    let unsigned = tx_request.build_unsigned().unwrap();

    let private = PrivateKeySigner::from_bytes(&GOLDEN_TOUCH_PRIVATE_KEY)?;
    let scalar = private.as_nonzero_scalar();

    match unsigned {
        TypedTransaction::Eip1559(tx) => {
            let signature = sign_fixed_k(tx.signature_hash(), scalar).expect("failed signature");
            let signed = tx.into_signed(signature);

            Ok(signed.into())
        }
        _ => unreachable!(),
    }
}
