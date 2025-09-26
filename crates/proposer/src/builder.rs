use std::time::Duration;

use alloy_consensus::TxEnvelope;
use alloy_eips::Encodable2718;
use alloy_primitives::{hex, B256};
use alloy_rpc_types::TransactionReceipt;
use pc_common::config::ProposerConfig;
use serde::{Deserialize, Serialize};
use tracing::{debug, info, warn};
use url::Url;

use crate::client::L1Client;

/// Response structure for the RPC server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcBundleResponse {
    pub jsonrpc: String,
    pub id: u32,
    pub result: Option<String>,
    pub error: Option<RpcBundleError>,
}

/// Error structure for the RPC server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcBundleError {
    pub code: i32,
    pub message: String,
}

pub fn bundle_request(tx_encoded: &str, block_number: u64) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_sendBundle",
        "params": [{
            "txs": [tx_encoded],
            "blockNumber": format!("0x{:x}", block_number),
        }]
    })
}

async fn send_bundle_request(
    url: &Url,
    tx_encoded: &str,
    block_number: u64,
) -> eyre::Result<RpcBundleResponse> {
    let client = reqwest::Client::new();
    let request =
        client.post(url.clone()).json(&bundle_request(tx_encoded, block_number)).send().await?;

    if !request.status().is_success() {
        warn!(request_status = ?request.status(), "Failed to send bundle");
        return Err(eyre::eyre!("Failed to send bundle: HTTP {}", request.status()));
    }

    let response: RpcBundleResponse = request.json().await?;
    if let Some(error) = &response.error {
        warn!(%error.code, %error.message, "RPC error occurred");
        return Err(eyre::eyre!("RPC Error {}: {}", error.code, error.message));
    }

    Ok(response)
}

pub async fn send_bundle(
    client: &L1Client,
    config: &ProposerConfig,
    tx: &TxEnvelope,
    tx_hash: B256,
) -> Option<TransactionReceipt> {
    match &config.builder_url {
        Some(url) => {
            let tx_encoded = format!("0x{}", hex::encode(tx.encoded_2718()));
            let start_block = client.get_last_block_number().await.ok()?;
            let _ = send_bundle_request(url, &tx_encoded, start_block + 1).await;
            let mut current_block = start_block;
            loop {
                if let Some(receipt) = client.get_tx_receipt(tx_hash).await.ok().flatten() {
                    info!(%tx_hash, "tx included via builder");
                    break Some(receipt);
                }
                let bn = client.get_last_block_number().await.ok()?;
                if bn > start_block + config.builder_max_retries {
                    debug!(%tx_hash, new_bn = bn, "giving up on builder. trying normal tx");
                    break None;
                }
                if bn != current_block {
                    debug!(%tx_hash, new_bn = bn, "resending bundle to builder");
                    let _ = send_bundle_request(url, &tx_encoded, bn + 1).await;
                    current_block = bn;
                }
                tokio::time::sleep(Duration::from_secs(6)).await;
                warn!(sent_bn = start_block, l1_bn = bn, %tx_hash, "waiting for receipt");
            }
        }
        None => None,
    }
}
