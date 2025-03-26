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
    use alloy_consensus::Transaction;
    use alloy_primitives::{address, b256, hex, Address, Bytes};
    use alloy_provider::{Provider, ProviderBuilder};
    use alloy_rpc_types::Block;
    use alloy_sol_types::{SolInterface, SolValue};

    use super::*;
    use crate::{
        config::{L2ChainConfig, TaikoChainParams},
        taiko::{
            pacaya::{
                decode_tx_list,
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

        let l2_provider = ProviderBuilder::new().on_http("http://abc.xyz".parse().unwrap());
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

        let parent_block: Block = serde_json::from_str(include_str!("1199.json")).expect("parent");
        assert_eq!(
            parent_block.header.hash,
            b256!("cd056f5b5c0f07c007bc7fdb22b0ad36a259219c1fb420795be03e13b18dd7a4")
        );
        assert_eq!(parent_block.header.number, 1199);

        let block: Block = serde_json::from_str(include_str!("1200.json")).expect("block");
        assert_eq!(
            block.header.hash,
            b256!("eaaa3d28a1148c4fca302e1b618387fd7002c30bc762808a64dba49b748bde66")
        );
        assert_eq!(block.header.number, 1200);

        let block_transactions = block.transactions.as_transactions().unwrap().to_vec();
        let block_anchor = block_transactions.first().unwrap();
        assert_eq!(block_anchor.from, GOLDEN_TOUCH_ADDRESS);
        assert_eq!(block_anchor.nonce(), parent_block.header.number);

        let anchor_call =
            anchorV3Call::abi_decode(&block_anchor.input(), true).expect("decode input");

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
            timestamp: parent_block.header.timestamp,
        };

        let parent_params = ParentParams {
            gas_used: parent_gas_used,
            block_number: parent_block.header.number,
            timestamp: parent_block.header.timestamp,
            hash: parent_block.header.hash,
        };

        let config = TaikoConfig {
            preconf_url: "http://abc.xyz".parse().unwrap(),
            config: L2ChainConfig {
                name: "".to_string(),
                rpc_url: "http://abc.xyz".parse().unwrap(),
                ws_url: "http://abc.xyz".parse().unwrap(),
                taiko_token: Address::ZERO,
                l1_contract: Address::ZERO,
                l2_contract: block_anchor.inner.to().unwrap(),
                router_contract: Address::ZERO,
                whitelist_contract: Address::ZERO,
                inclusion_store_address: Address::ZERO,
                taiko_wrapper_address: Address::ZERO,
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
            serde_json::from_str(include_str!("1478988.json")).expect("block");
        assert_eq!(
            anchor_block.header.hash,
            b256!("4482999a8813eb4f7fef7db09c4ecc8551e7eb194e31bea490e794f035637de0")
        );
        assert_eq!(anchor_block.header.number, 1478988);

        let propose_block: Block =
            serde_json::from_str(include_str!("1478998.json")).expect("block");
        assert_eq!(
            propose_block.header.hash,
            b256!("9621c396d59b9c9de72d8015cc2c873e269eafdcb6ab9dc1cf6d6790cb9cda1b")
        );
        assert_eq!(propose_block.header.number, 1478998);

        let l2_block: Block = serde_json::from_str(include_str!("1200.json")).expect("block");
        assert_eq!(
            l2_block.header.hash,
            b256!("eaaa3d28a1148c4fca302e1b618387fd7002c30bc762808a64dba49b748bde66")
        );
        assert_eq!(l2_block.header.number, 1200);

        let anchor_tx = l2_block.transactions.txns().next().unwrap().clone();
        assert_eq!(anchor_tx.from, GOLDEN_TOUCH_ADDRESS);

        let anchor_call = anchorV3Call::abi_decode(&anchor_tx.input(), true).expect("decode input");

        assert_eq!(anchor_call._anchorStateRoot, anchor_block.header.state_root);

        let propose_tx = propose_block
            .transactions
            .txns()
            .cloned()
            .collect::<Vec<_>>()
            .into_iter()
            .find(|tx| tx.from == address!("F3384dCC14F03f079Ac7cd3C2299256B19261Bb0"))
            .unwrap();

        let input = propose_tx.input();
        let call = proposeBatchCall::abi_decode(&input, true).expect("decode input");

        let tx_list = decode_tx_list(&call._txList)
            .unwrap()
            .into_iter()
            .map(|tx| *tx.tx_hash())
            .collect::<Vec<_>>();

        // skip anchor
        assert!(l2_block.transactions.hashes().skip(1).all(|h| tx_list.contains(&h)));

        let (_forced, params_bytes) =
            <(Bytes, Bytes)>::abi_decode_params(&call._params, true).unwrap();

        let params = <BatchParams as SolValue>::abi_decode(&params_bytes, true).unwrap();

        assert_eq!(params.anchorBlockId, anchor_block.header.number);
    }

    #[ignore]
    #[tokio::test]
    async fn test_fetch_block() {
        let provider =
            ProviderBuilder::new().on_http("http://18.199.195.154:8545".parse().unwrap());

        let bn = provider.get_block_by_number(1478998.into(), true.into()).await.unwrap().unwrap();

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

        let err = TaikoL2Errors::abi_decode(&bytes, true).unwrap();
        println!("{:?}", err);
    }

    #[ignore]
    #[test]
    fn test_decode_l1_error() {
        let bytes = hex!("fe1698b2");

        let err = TaikoL1Errors::abi_decode(&bytes, true).unwrap();
        println!("{:?}", err);
    }

    #[ignore]
    #[test]
    fn test_decode_router_error() {
        let bytes = hex!("47fac6c1");

        let err = PreconfRouterErrors::abi_decode(&bytes, true).unwrap();
        println!("{:?}", err);
    }

    #[ignore]
    #[test]
    fn test_decode_whitelist_error() {
        let bytes = hex!("83738f36");

        let err = PreconfWhitelistErrors::abi_decode(&bytes, true).unwrap();
        println!("{:?}", err);
    }
}
