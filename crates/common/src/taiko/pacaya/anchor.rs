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
    timestamp: u64,
    taiko_l2: &TaikoL2Client,
    base_fee_config: BaseFeeConfig,
    parent: ParentParams,
) -> eyre::Result<u128> {
    assert!(timestamp >= parent.timestamp);

    let base_fee = taiko_l2
        .getBasefeeV2(parent.gas_used, timestamp, base_fee_config.into())
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
) -> TxEnvelope {
    const SIGNAL_SLOTS: Vec<B256> = vec![];

    let input = anchorV3Call::new((
        anchor.block_id,
        anchor.state_root,
        parent.gas_used,
        config.params.base_fee_config.into(),
        SIGNAL_SLOTS,
    ))
    .abi_encode();

    let tx = TxEip1559 {
        chain_id: config.params.chain_id,
        nonce: parent.anchor_nonce + 1,
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
    use alloy_consensus::Transaction;
    use alloy_network::TransactionResponse;
    use alloy_primitives::{address, b256, hex, Address, Bytes};
    use alloy_provider::{Provider, ProviderBuilder};
    use alloy_rpc_types::Block;
    use alloy_sol_types::{SolInterface, SolType};
    use url::Url;

    use super::*;
    use crate::{
        config::{L2ChainConfig, TaikoChainParams},
        taiko::{
            pacaya::{
                l1::TaikoL1::{proposeBatchCall, TaikoL1Errors},
                l2::TaikoL2::{self, TaikoL2Errors},
                preconf::{
                    PreconfRouter::PreconfRouterErrors, PreconfWhitelist::PreconfWhitelistErrors,
                },
                BatchParams,
            },
            GOLDEN_TOUCH_ADDRESS,
        },
    };

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

        let l2_provider = ProviderBuilder::new().connect_http("http://abc.xyz".parse().unwrap());
        let taiko_l2 = address!("1234567890abcdef1234567890abcdef12345678");

        let taiko_l2 = TaikoL2::new(taiko_l2, l2_provider);

        let input = taiko_l2
            .anchorV3(
                anchor_block_id,
                anchor_state_root,
                parent_gas_used,
                base_fee_config.into(),
                signal_slots.clone(),
            )
            .calldata()
            .clone();

        let input_check = anchorV3Call::new((
            anchor_block_id,
            anchor_state_root,
            parent_gas_used,
            base_fee_config.into(),
            signal_slots,
        ))
        .abi_encode();

        assert_eq!(input, Bytes::from(input_check));
    }

    #[tokio::test]
    async fn test_validate_anchor_v3() {
        let base_fee_config = TaikoChainParams::new_helder().base_fee_config;

        let parent_block: Block = serde_json::from_str(include_str!("750.json")).expect("parent");
        assert_eq!(
            parent_block.header.hash,
            b256!("d9a5648a447658d9f51846d546a2cd56f0abe39a7d7910f0f43ee2ea054b94ba")
        );
        assert_eq!(parent_block.header.number, 750);

        let block: Block = serde_json::from_str(include_str!("751.json")).expect("block");
        assert_eq!(
            block.header.hash,
            b256!("58bcf2dfc2a67e820f30e7f320d8e3c532629c5411856a401c60883d91d22971")
        );
        assert_eq!(block.header.number, 751);

        let block_transactions = block.transactions.as_transactions().unwrap().to_vec();
        let block_anchor = block_transactions.first().unwrap();
        assert_eq!(block_anchor.from(), GOLDEN_TOUCH_ADDRESS);
        assert_eq!(block_anchor.nonce(), parent_block.header.number);

        let anchor_call = anchorV3Call::abi_decode(&block_anchor.input()).expect("decode input");

        let anchor_block_id = anchor_call._anchorBlockId;
        let anchor_state_root = anchor_call._anchorStateRoot;
        let parent_gas_used = anchor_call._parentGasUsed;
        let base_fee_cfg = anchor_call._baseFeeConfig;

        assert_eq!(parent_gas_used as u64, parent_block.header.gas_used);

        assert_eq!(base_fee_cfg.adjustmentQuotient, base_fee_config.adjustment_quotient);
        assert_eq!(base_fee_cfg.gasIssuancePerSecond, base_fee_config.gas_issuance_per_second);
        assert_eq!(base_fee_cfg.maxGasIssuancePerBlock, base_fee_config.max_gas_issuance_per_block);
        assert_eq!(base_fee_cfg.minGasExcess, base_fee_config.min_gas_excess);
        assert_eq!(base_fee_cfg.sharingPctg, base_fee_config.sharing_pctg);

        let anchor_params = AnchorParams {
            block_id: anchor_block_id,
            state_root: anchor_state_root,
            created_at: 0,
        };

        let parent_params = ParentParams {
            gas_used: parent_gas_used,
            block_number: parent_block.header.number,
            timestamp: parent_block.header.timestamp,
            hash: parent_block.header.hash,
            anchor_nonce: parent_block.header.number,
            beneficiary: Address::ZERO,
        };

        let config = TaikoConfig {
            preconf_url: "http://abc.xyz".parse().unwrap(),
            config: L2ChainConfig {
                name: "".to_string(),
                rpc_url: Url::parse("http://abc.xyz").unwrap().into(),
                ws_url: Url::parse("http://abc.xyz").unwrap().into(),
                taiko_token: Address::ZERO,
                l1_contract: Address::ZERO,
                l2_contract: block_anchor.inner.to().unwrap(),
                router_contract: Address::ZERO,
                whitelist_contract: Address::ZERO,
                wrapper_contract: Address::ZERO,
            },
            params: TaikoChainParams::new_helder(),
        };

        // // NOTE: this is commented out as the call is to a stateful contract so may fail
        // let url: url::Url = "https://rpc.helder.taiko.xyz".parse().unwrap();
        // let provider = ProviderBuilder::new().disable_recommended_fillers().on_http(url);
        // let taiko_l2 = TaikoL2::new(config.l2_contract, provider);

        // let base_fee =
        //     compute_next_base_fee(&taiko_l2, base_fee_config, parent_params, anchor_params)
        //         .await
        //         .unwrap();

        // assert_eq!(base_fee, block.header.base_fee_per_gas.unwrap() as u128);

        let test_anchor = assemble_anchor_v3(
            &config,
            parent_params,
            anchor_params,
            block.header.base_fee_per_gas.unwrap() as u128,
        );

        assert_eq!(test_anchor.tx_hash(), block_anchor.inner.tx_hash())
    }

    #[test]
    fn test_decode_propose_block() {
        let anchor_block: Block =
            serde_json::from_str(include_str!("1275729.json")).expect("block");
        // assert_eq!(
        //     anchor_block.header.hash,
        //     b256!("d7f49b0a0b22969d2640172cb4fcb93bef05793c93ebb07c4bfc2eb647d0a2f51")
        // );
        assert_eq!(anchor_block.header.number, 1275729);

        let propose_block: Block =
            serde_json::from_str(include_str!("1275730.json")).expect("block");
        assert_eq!(
            propose_block.header.hash,
            b256!("d0a1c88b01959e435f972cc46707aec2567cec1f964159b89ad0e4e873c8bdb7")
        );
        assert_eq!(propose_block.header.number, 1275730);

        let l2_block: Block = serde_json::from_str(include_str!("751.json")).expect("block");
        assert_eq!(
            l2_block.header.hash,
            b256!("ccdd76f4a02856cfdd33ad52be94e32acdb0260f5bc682706bda6c2b3ab562b9")
        );
        assert_eq!(l2_block.header.number, 5);

        let anchor_tx = l2_block.transactions.txns().next().unwrap().clone();
        assert_eq!(anchor_tx.from(), GOLDEN_TOUCH_ADDRESS);

        let anchor_call = anchorV3Call::abi_decode(&anchor_tx.input()).expect("decode input");

        assert_eq!(anchor_call._anchorStateRoot, anchor_block.header.state_root);

        let propose_tx = propose_block
            .transactions
            .txns()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .find(|tx| tx.to() == Some(address!("55FC9869E51885B6523D25d20A82A4bD9787998F")))
            .unwrap();

        let input = propose_tx.input();
        let call = proposeBatchCall::abi_decode(&input).expect("decode input");

        let params = call._params;

        assert!(BatchParams::abi_decode(&params).is_ok());
    }

    #[ignore]
    #[tokio::test]
    async fn test_fetch_block() {
        let provider =
            ProviderBuilder::new().connect_http("http://18.199.195.154:18545".parse().unwrap());

        let bn = provider.get_block_by_number(751.into()).await.unwrap().unwrap();

        let s = serde_json::to_string(&bn).unwrap();
        println!("{}", s);

        // let provider = ProviderBuilder::new().on_http("http://0.0.0.0:8545".parse().unwrap());

        // let bn = provider.get_block_by_number(47.into(), true.into()).await.unwrap().unwrap();

        // let s = serde_json::to_string(&bn).unwrap();
        // println!("{}", s);
    }

    #[ignore]
    #[test]
    fn test_decode_l2_error() {
        let bytes = hex!("d719258d");

        let err = TaikoL2Errors::abi_decode(&bytes).unwrap();
        println!("{:?}", err);
    }

    #[ignore]
    #[test]
    fn test_decode_l1_error() {
        let bytes = hex!("fe1698b2");

        let err = TaikoL1Errors::abi_decode(&bytes).unwrap();
        println!("{:?}", err);
    }

    #[ignore]
    #[test]
    fn test_decode_router_error() {
        let bytes = hex!("47fac6c1");

        let err = PreconfRouterErrors::abi_decode(&bytes).unwrap();
        println!("{:?}", err);
    }

    #[ignore]
    #[test]
    fn test_decode_whitelist_error() {
        let bytes = hex!("83738f36");

        let err = PreconfWhitelistErrors::abi_decode(&bytes).unwrap();
        println!("{:?}", err);
    }
}
