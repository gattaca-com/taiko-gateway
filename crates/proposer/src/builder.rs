use serde::{Deserialize, Serialize};
use tracing::{info, warn};
use url::Url;

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

pub async fn send_bundle_request(
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

    info!(result = ?response.result, ?block_number, "Bundle sent successfully");
    Ok(response)
}
