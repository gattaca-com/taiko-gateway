#[cfg(test)]
mod tests {

    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy_eips::eip2718::Encodable2718;
    use alloy_primitives::{b256, utils::parse_ether, Bytes, U256};
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

    #[tokio::test]
    async fn test_spam_many() {
        let full_rpc = std::env::var("FULL_RPC").expect("FULL_RPC").parse::<Url>().unwrap();
        let config = std::env::var("GATEWAY_RPC").expect("GATEWAY_RPC").parse::<Url>().unwrap();
        let private_key =
            std::env::var("SENDER_KEY").expect("SENDER_KEY").parse::<PrivateKeySigner>().unwrap();

        let mut signers = Vec::new();
        let signer = PrivateKeySigner::from_bytes(&b256!(
            "b5c298ae1dad275b2d9c74b5d327852d0c661de19bbbb3f46ed9b5a3bc2bf0e7"
        ))
        .unwrap();
        signers.push(signer);
        let signer = PrivateKeySigner::from_bytes(&b256!(
            "b5c298ae1dad275b2d9c74b5d327852d0c661de19bbbb3f46ed9b5a3bc2bf0e8"
        ))
        .unwrap();
        signers.push(signer);
        let signer = PrivateKeySigner::from_bytes(&b256!(
            "b5c298ae1dad275b2d9c74b5d327852d0c661de19bbbb3f46ed9b5a3bc2bf0e9"
        ))
        .unwrap();
        signers.push(signer);

        println!("Sending funding test tx from: {}", private_key.address());

        let full_provider = ProviderBuilder::new().disable_recommended_fillers().on_http(full_rpc);
        let send_tx_provider = ProviderBuilder::new().disable_recommended_fillers().on_http(config);

        let chain_id = full_provider.get_chain_id().await.unwrap();

        let _transfer_amount = parse_ether("0.05").unwrap();
        // let nonce = full_provider.get_transaction_count(private_key.address()).await.unwrap();

        // for (i, signer) in signers.iter().enumerate() {
        //     let to_address = signer.address();
        //     let priority_fee = 80 + 20 * (i + 1) as u128;
        //     let tx = TxEip1559 {
        //         chain_id,
        //         nonce: nonce + i as u64,
        //         gas_limit: 21_000,
        //         max_fee_per_gas: 100_000_000,
        //         max_priority_fee_per_gas: priority_fee,
        //         to: to_address.into(),
        //         value: transfer_amount,
        //         access_list: AccessList::default(),
        //         input: Bytes::new(),
        //     };

        //     let sig = private_key.sign_hash_sync(&tx.signature_hash()).unwrap();
        //     let tx: TxEnvelope = tx.into_signed(sig).into();
        //     println!("Sending tx: {} with priority fee: {priority_fee}", tx.tx_hash());
        //     let encoded = tx.encoded_2718();
        //     let _pending = send_tx_provider.send_raw_transaction(&encoded).await.unwrap();
        // }

        println!("Sending spam txs");

        for (i, signer) in signers.iter().enumerate() {
            let nonce = full_provider.get_transaction_count(signer.address()).await.unwrap();
            let to_address = signer.address();
            // let priority_fee = 100 + 60 * (i + 1) as u128;
            let priority_fee = 1 + i as u128;
            for j in 0..30 {
                let tx = TxEip1559 {
                    chain_id,
                    nonce: nonce + j as u64,
                    gas_limit: 21_000,
                    max_fee_per_gas: 100_000_000,
                    max_priority_fee_per_gas: priority_fee,
                    to: to_address.into(),
                    value: U256::from(1),
                    access_list: AccessList::default(),
                    input: Bytes::new(),
                };

                let sig = signer.sign_hash_sync(&tx.signature_hash()).unwrap();
                let tx: TxEnvelope = tx.into_signed(sig).into();
                println!("Sending tx: {} with priority fee: {priority_fee}", tx.tx_hash());
                let encoded = tx.encoded_2718();
                let _pending = send_tx_provider.send_raw_transaction(&encoded).await.unwrap();
            }
        }
    }
}
