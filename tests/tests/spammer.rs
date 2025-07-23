#[cfg(test)]
mod tests {
    use eyre;
    use std::time::{Duration, Instant};

    use alloy_consensus::{SignableTransaction, TxEip1559, TxEnvelope};
    use alloy_eips::eip2718::Encodable2718;
    use alloy_primitives::{b256, utils::format_ether, Bytes, B256, U256};
    use alloy_provider::{Provider, ProviderBuilder, RootProvider};
    use alloy_rpc_types::AccessList;
    use alloy_signer::SignerSync;
    use alloy_signer_local::PrivateKeySigner;
    use reqwest::Url;

    #[tokio::test]
    async fn test_send_locally() {
        let full_rpc = "http://0.0.0.0:8545".parse::<Url>().unwrap();
        let private_key = "350ea19e5e03cc28e3e4f1570feb9343fe90cdf9746f1adf4993f3bca9b57f19"
            .parse::<PrivateKeySigner>()
            .unwrap();

        println!("Sending test tx from: {}", private_key.address());

        let provider = ProviderBuilder::new().disable_recommended_fillers().on_http(full_rpc);

        let chain_id = provider.get_chain_id().await.unwrap();
        let nonce = provider.get_transaction_count(private_key.address()).await.unwrap();

        let tx = TxEip1559 {
            chain_id,
            nonce: nonce + 1,
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

        let _pending_tx = provider.send_raw_transaction(&encoded).await.unwrap();
    }

    #[tokio::test]
    async fn test_spam_many() {
        let full_rpc = "http://0.0.0.0:8545".parse::<Url>().unwrap();

        let private_key = "350ea19e5e03cc28e3e4f1570feb9343fe90cdf9746f1adf4993f3bca9b57f19"
            .parse::<PrivateKeySigner>()
            .unwrap();


        let n_wallets = 5;

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

        for i in 0..n_wallets {
            let secret = B256::repeat_byte(i as u8 + 1);
            let signer = PrivateKeySigner::from_bytes(&secret).unwrap();
            signers.push(signer);
        }

        println!("Sending funding test tx from: {}", private_key.address());

        let provider =
            ProviderBuilder::new().disable_recommended_fillers().on_http(full_rpc.clone());

        let chain_id = provider.get_chain_id().await.unwrap();

        // let transfer_amount = parse_ether("0.01").unwrap();
        // let nonce = provider.get_transaction_count(private_key.address()).await.unwrap();

        // for (i, signer) in signers.iter().enumerate() {
        //     let to_address = signer.address();
        //     let priority_fee = 20 * (i + 1) as u128;
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
        //     let _pending = provider.send_raw_transaction(&encoded).await.unwrap();
        // }

        println!("Sending spam txs");

        let mut j = 0;
        loop {
            if j % 30 == 0 {
                for signer in &signers {
                    // print balance
                    let balance = provider.get_balance(signer.address()).await.unwrap();
                    println!("Balance of {}: {}", signer.address(), format_ether(balance));
                }
            }

            let start = Instant::now();
            for (i, signer) in signers.iter().enumerate() {
                let url = full_rpc.clone();
                let signer = signer.clone();
                tokio::spawn(async move {
                    let provider =
                        ProviderBuilder::new().disable_recommended_fillers().on_http(url);

                    let nonce = provider.get_transaction_count(signer.address()).await.unwrap();
                    let to_address = signer.address();
                    // let priority_fee = 100 + 60 * (i + 1) as u128;
                    let priority_fee = 1 + i as u128;

                    let tx = TxEip1559 {
                        chain_id,
                        nonce,
                        gas_limit: 21_000,
                        max_fee_per_gas: 10_000_000,
                        max_priority_fee_per_gas: priority_fee,
                        to: to_address.into(),
                        value: U256::from(1),
                        access_list: AccessList::default(),
                        input: Bytes::new(),
                    };

                    let sig = signer.sign_hash_sync(&tx.signature_hash()).unwrap();
                    let tx: TxEnvelope = tx.into_signed(sig).into();
                    // println!("Sending tx: {} with priority fee: {priority_fee}", tx.tx_hash());
                    let encoded = tx.encoded_2718();
                    let _pending = provider.send_raw_transaction(&encoded).await.unwrap();
                });
            }

            println!("Sent {} txs in {:?}", signers.len(), start.elapsed());
            tokio::time::sleep(Duration::from_millis(2000)).await;
            j += 1;
        }
    }

    #[tokio::test]
    async fn test_send_self() {
        use alloy_primitives::utils::parse_ether;

        let full_rpc = "https://ethereum-holesky-rpc.publicnode.com".parse::<Url>().unwrap();
        let private_key = "764ba168c959e7a211f64e0889ce5eeaec0434898a08876ca631f9ec746ffa64"
            .parse::<PrivateKeySigner>()
            .unwrap();

        let provider =
            ProviderBuilder::new().disable_recommended_fillers().on_http(full_rpc.clone());

        let chain_id = provider.get_chain_id().await.unwrap();

        let transfer_amount = parse_ether("1").unwrap();
        let nonce = provider.get_transaction_count(private_key.address()).await.unwrap();

        println!("Sending tx to self with nonce: {}", nonce);

        let to_address = private_key.address();
        let tx = TxEip1559 {
            chain_id,
            nonce,
            gas_limit: 21_000,
            max_fee_per_gas: 100_000_000,
            max_priority_fee_per_gas: 5000,
            to: to_address.into(),
            value: transfer_amount,
            access_list: AccessList::default(),
            input: Bytes::new(),
        };

        let sig = private_key.sign_hash_sync(&tx.signature_hash()).unwrap();
        let tx: TxEnvelope = tx.into_signed(sig).into();
        println!("Sending tx: {}", tx.tx_hash());
        let encoded = tx.encoded_2718();
        let receipt = provider.send_raw_transaction(&encoded).await.unwrap().watch().await.unwrap();
        println!("Receipt: {:?}", receipt);
    }

    #[tokio::test]
    async fn test_self_spam() {
        use alloy_primitives::utils::parse_ether;

        let full_rpc = "http://51.89.23.230:8545".parse::<Url>().unwrap();
        let private_key = "350ea19e5e03cc28e3e4f1570feb9343fe90cdf9746f1adf4993f3bca9b57f19"
            .parse::<PrivateKeySigner>()
            .unwrap();

        let provider =
            ProviderBuilder::new().disable_recommended_fillers().on_http(full_rpc.clone());

        let chain_id = provider.get_chain_id().await.unwrap();

        println!("Sending spam txs");

        let transfer_amount = parse_ether("0.25").unwrap();
        let nonce = provider.get_transaction_count(private_key.address()).await.unwrap();

        let to_address = private_key.address();
        let tx = TxEip1559 {
            chain_id,
            nonce,
            gas_limit: 21_000,
            max_fee_per_gas: 100_000_000,
            max_priority_fee_per_gas: 100,
            to: to_address.into(),
            value: transfer_amount,
            access_list: AccessList::default(),
            input: Bytes::new(),
        };

        let sig = private_key.sign_hash_sync(&tx.signature_hash()).unwrap();
        let tx: TxEnvelope = tx.into_signed(sig).into();
        println!("Sending tx: {}", tx.tx_hash());
        let encoded = tx.encoded_2718();
        let _pending = provider.send_raw_transaction(&encoded).await.unwrap();
    }

    #[tokio::test]
    async fn test_fund_spam() {
        use alloy_primitives::utils::parse_ether;

        let full_rpc = "http://51.89.23.230:8545".parse::<Url>().unwrap();
        let private_key = "350ea19e5e03cc28e3e4f1570feb9343fe90cdf9746f1adf4993f3bca9b57f19"
            .parse::<PrivateKeySigner>()
            .unwrap();

        let provider =
            ProviderBuilder::new().disable_recommended_fillers().on_http(full_rpc.clone());

        let chain_id = provider.get_chain_id().await.unwrap();

        let mut signers = Vec::new();

        for i in 0..50 {
            let secret = B256::repeat_byte(i as u8 + 1);
            let signer = PrivateKeySigner::from_bytes(&secret).unwrap();
            signers.push(signer);
        }

        println!("Sending spam txs");

        let transfer_amount = parse_ether("0.25").unwrap();
        let nonce = provider.get_transaction_count(private_key.address()).await.unwrap();

        for (i, signer) in signers.iter().enumerate() {
            let to_address = signer.address();
            let tx = TxEip1559 {
                chain_id,
                nonce: nonce + i as u64,
                gas_limit: 21_000,
                max_fee_per_gas: 100_000_000,
                max_priority_fee_per_gas: 100,
                to: to_address.into(),
                value: transfer_amount,
                access_list: AccessList::default(),
                input: Bytes::new(),
            };

            let sig = private_key.sign_hash_sync(&tx.signature_hash()).unwrap();
            let tx: TxEnvelope = tx.into_signed(sig).into();
            println!("Sending tx: {}", tx.tx_hash());
            let encoded = tx.encoded_2718();
            let _pending = provider.send_raw_transaction(&encoded).await.unwrap();
        }
    }

    #[tokio::test]
    async fn test_spam_loop() {
        // let full_rpc = std::env::var("RPC_URL").expect("RPC_URL").parse::<Url>().unwrap();
        // let n_wallets = std::env::var("N_WALLETS").expect("N_WALLETS").parse::<u32>().unwrap();

        let full_rpc = "http://51.89.23.230:8545".parse::<Url>().unwrap();
        let n_wallets = 5;

        let mut signers = Vec::new();

        for i in 0..n_wallets {
            let secret = B256::repeat_byte(i as u8 + 1);
            let signer = PrivateKeySigner::from_bytes(&secret).unwrap();
            signers.push(signer);
        }

        let provider =
            ProviderBuilder::new().disable_recommended_fillers().on_http(full_rpc.clone());

        let chain_id = provider.get_chain_id().await.unwrap();

        println!("Sending spam txs");

        for (i, signer) in signers.into_iter().enumerate() {
            let provider = provider.clone();

            tokio::spawn(async move {
                // sleep pseudo-randomly between 1 and 4000ms
                tokio::time::sleep(Duration::from_millis(1000 * (1 + (i as u64 % 4)))).await;
                let mut j = 0;
                loop {
                    if let Err(err) = send_one_tx(chain_id, &signer, &provider).await {
                        eprintln!("Error sending tx: {err}");
                        tokio::time::sleep(Duration::from_secs(12)).await;
                    } else {
                        tokio::time::sleep(Duration::from_secs(4)).await;
                    }

                    if j % 30 == 0 {
                        let balance =
                            provider.get_balance(signer.address()).await.unwrap_or(U256::ZERO);
                        println!("Balance of {}: {}", signer.address(), format_ether(balance));
                    }

                    j += 1;
                }
            });
        }

        tokio::time::sleep(Duration::from_secs(u64::MAX)).await;
    }

    async fn send_one_tx(
        chain_id: u64,
        signer: &PrivateKeySigner,
        provider: &RootProvider,
    ) -> eyre::Result<()> {
        let nonce = provider.get_transaction_count(signer.address()).await?;
        let to_address = signer.address();

        for i in 0..4 {
            let tx = TxEip1559 {
                chain_id,
                nonce: nonce + i,
                gas_limit: 21_000,
                max_fee_per_gas: 10_000_000,
                max_priority_fee_per_gas: 10,
                to: to_address.into(),
                value: U256::from(1),
                access_list: AccessList::default(),
                input: Bytes::new(),
            };

            let sig = signer.sign_hash_sync(&tx.signature_hash())?;
            let tx: TxEnvelope = tx.into_signed(sig).into();
            let encoded = tx.encoded_2718();
            println!("{} tx with nonce: {}", signer.address(), nonce + i);
            let _ = provider.send_raw_transaction(&encoded).await;

            tokio::time::sleep(Duration::from_millis(100)).await;
        }

        Ok(())
    }
}
