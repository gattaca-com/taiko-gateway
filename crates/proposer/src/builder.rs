use alloy_primitives::B256;
use eyre::bail;
use serde::{Deserialize, Serialize};
use tracing::info;
use url::Url;

/// Response structure for the RPC server
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcBundleResponse {
    pub result: Option<BundleResult>,
    pub error: Option<RpcBundleError>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BundleResult {
    bundle_hash: B256,
}

/// Error structure for the RPC server
#[derive(Debug, Clone, Serialize, Deserialize)]
struct RpcBundleError {
    pub code: i32,
    pub message: String,
}

fn bundle_request(tx_encoded: &str, target_block: u64) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "eth_sendBundle",
        "params": [{
            "txs": [tx_encoded],
            "blockNumber": format!("0x{:x}", target_block),
        }]
    })
}

pub async fn send_bundle_request(
    url: &Url,
    tx_encoded: &str,
    target_block: u64,
) -> eyre::Result<()> {
    let client = reqwest::Client::new();
    let request =
        client.post(url.clone()).json(&bundle_request(tx_encoded, target_block)).send().await?;

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

    let Some(result) = response.result else {
        bail!("missing result and error, shouldn't happen: response: {response:?}");
    };

    info!(bundle_hash = %result.bundle_hash, target_block, "bundle sent successfully");

    Ok(())
}
