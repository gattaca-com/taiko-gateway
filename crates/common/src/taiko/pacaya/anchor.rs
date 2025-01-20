use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
use alloy_primitives::{B256, U256};
use alloy_sol_types::SolCall;

use super::l2::TaikoL2::anchorV3Call;
use crate::{
    config::TaikoConfig,
    taiko::{
        fixed_signer::sign_fixed_k, AnchorParams, BaseFeeConfig, ParentParams, TaikoL2Client,
        ANCHOR_GAS_LIMIT, GOLDEN_TOUCH_SIGNER,
    },
};

/// Return base fee for the next block, this is verified in the anchorV3 calls
/// Note that the URL passed need to have the preconf state, as some fields are read from the
/// program state
// https://github.com/taikoxyz/taiko-mono/blob/pacaya_fork/packages/protocol/contracts/layer2/based/TaikoAnchor.sol#L206
pub async fn compute_next_base_fee(
    taiko_l2: &TaikoL2Client,
    base_fee_config: BaseFeeConfig,
    parent: ParentParams,
    anchor: AnchorParams,
) -> eyre::Result<u128> {
    assert!(anchor.timestamp >= parent.timestamp);

    let base_fee = taiko_l2
        .getBasefeeV2(parent.gas_used, anchor.timestamp, base_fee_config.into())
        .call()
        .await?
        .basefee_;

    Ok(base_fee.try_into()?)
}

/// Assemble the anchorV3 transaction and sign it with the golden touch private key
// https://github.com/taikoxyz/taiko-geth/blob/preconfs/consensus/taiko/consensus.go#L284
pub fn assemble_anchor_v3(
    config: &TaikoConfig,
    parent: ParentParams,
    anchor: AnchorParams,
    l2_base_fee: u128, // base fee where this tx will be included
    anchor_input: B256,
) -> TxEnvelope {
    const SIGNAL_SLOTS: Vec<B256> = vec![];

    let input = anchorV3Call::new((
        anchor.block_id,
        anchor.state_root,
        anchor_input,
        parent.gas_used,
        config.params.base_fee_config.into(),
        SIGNAL_SLOTS,
    ))
    .abi_encode();

    let tx = TxEip1559 {
        chain_id: config.chain_id,
        nonce: parent.block_number, // one anchor tx per L2 block -> block number = nonce
        gas_limit: ANCHOR_GAS_LIMIT,
        max_fee_per_gas: l2_base_fee,
        max_priority_fee_per_gas: 0,
        to: config.l2_contract.into(),
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
    use alloy_provider::ProviderBuilder;

    use super::*;
    use crate::{config::TaikoChainParams, taiko::pacaya::l2::TaikoL2};

    #[test]
    fn test_input() {
        let anchor_block_id = 1;
        let anchor_state_root =
            b256!("1234567890abcdef1234567890abcdef1234567890abcdef1234567890abcdef");
        let parent_gas_used = 100;
        let base_fee_config = TaikoChainParams::new_helder().base_fee_config;
        let anchor_input =
            b256!("abcdef1234567890abcdef1234567890abcdef1234567890abcdef1234567890");
        let signal_slots = vec![anchor_state_root, anchor_input];

        let l2_provider = ProviderBuilder::new().on_http("http://abc.xyz".parse().unwrap());
        let taiko_l2 = address!("1234567890abcdef1234567890abcdef12345678");

        let taiko_l2 = TaikoL2::new(taiko_l2, l2_provider);

        let input = taiko_l2
            .anchorV3(
                anchor_block_id,
                anchor_state_root,
                anchor_input,
                parent_gas_used,
                base_fee_config.into(),
                signal_slots.clone(),
            )
            .calldata()
            .clone();

        let input_check = anchorV3Call::new((
            anchor_block_id,
            anchor_state_root,
            anchor_input,
            parent_gas_used,
            base_fee_config.into(),
            signal_slots,
        ))
        .abi_encode();

        assert_eq!(input, Bytes::from(input_check));
    }
}
