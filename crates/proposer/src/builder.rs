use eyre::bail;
use serde::{Deserialize, Serialize};
use tracing::info;
use url::Url;

/// Response structure for the RPC server
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RpcBundleResponse {
    pub result: Option<serde_json::Value>,
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

    let status = request.status();
    let body = request.bytes().await?;

    if !status.is_success() {
        bail!("failed to send bundle: code: {}, body: {body:?}", status);
    }

    let Ok(response) = serde_json::from_slice::<RpcBundleResponse>(body.as_ref()) else {
        bail!("failed to decode response body: raw: {body:?}")
    };

    if let Some(error) = &response.error {
        bail!("RPC Error {}: {}", error.code, error.message);
    }

    info!(result = ?response.result, ?block_number, "Bundle sent successfully");
    Ok(response)
}
