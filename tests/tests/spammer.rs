#[cfg(test)]
mod tests {

    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy_eips::eip2718::Encodable2718;
    use alloy_primitives::{Bytes, U256};
    use alloy_provider::{Provider, ProviderBuilder};
    use alloy_rpc_types::AccessList;
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use reqwest::Url;

    #[tokio::test]
    async fn test_send_locally() {
        let full_rpc = std::env::var("FULL_RPC").expect("FULL_RPC").parse::<Url>().unwrap();
        let config = std::env::var("GATEWAY_RPC").expect("GATEWAY_RPC").parse::<Url>().unwrap();
        let private_key =
            std::env::var("SENDER_KEY").expect("SENDER_KEY").parse::<PrivateKeySigner>().unwrap();

        println!("Sending test tx from: {}", private_key.address());

        let full_provider = ProviderBuilder::new().disable_recommended_fillers().on_http(full_rpc);
        let send_tx_provider = ProviderBuilder::new().disable_recommended_fillers().on_http(config);

        let chain_id = full_provider.get_chain_id().await.unwrap();
        let nonce = full_provider.get_transaction_count(private_key.address()).await.unwrap();

        let tx = TxEip1559 {
            chain_id,
            nonce,
            gas_limit: 21_000,
            max_fee_per_gas: 100_000_000,
            max_priority_fee_per_gas: 1,
            to: private_key.address().into(),
            value: U256::from(1u64),
            access_list: AccessList::default(),
            input: Bytes::new(),
        };

        let sig = private_key.sign_hash_sync(&tx.signature_hash()).unwrap();
        let tx: TxEnvelope = tx.into_signed(sig).into();
        let encoded = tx.encoded_2718();

        println!("Sending tx: {}", tx.tx_hash());

        let _pending_tx = send_tx_provider.send_raw_transaction(&encoded).await.unwrap();
    }
}
