
use tokio::sync::mpsc::Sender;
use tokio_tungstenite::{connect_async, tungstenite::protocol::Message};
use futures_util::{StreamExt, SinkExt};
use url::Url;
use eyre::Result;
use serde_json::json;
use tokio::time::{sleep, Duration};
use log::info;

pub async fn subscribe_mempool(rpc_url: Url, mempool_tx: Sender<Order>) -> Result<()> {
    info!(rpc_url = %rpc_url, "subscribing to mempool");

    loop {
        match connect_async(rpc_url.clone()).await {
            Ok((mut ws_stream, _)) => {
                info!("Connected to WebSocket server");

                // Send subscription request
                let sub_msg = json!({
                    "jsonrpc": "2.0",
                    "method": "eth_subscribe",
                    "params": ["newPendingTransactions"],
                    "id": 1
                }).to_string();

                ws_stream.send(Message::Text(sub_msg)).await?;

                while let Some(msg) = ws_stream.next().await {
                    match msg {
                        Ok(Message::Text(text)) => {
                            // Process the transaction hash
                            if let Ok(tx_hash) = serde_json::from_str::<serde_json::Value>(&text) {
                                if let Some(hash) = tx_hash["result"].as_str() {
                                    MempoolMetrics::tx_received();
                                    info!(hash = %hash, "received from mempool");
                                    let _ = mempool_tx.send(Order::new_with_sender(hash.to_string(), "unknown".to_string())).await;
                                }
                            }
                        }
                        Ok(Message::Close(_)) => {
                            info!("WebSocket connection closed. Attempting to reconnect...");
                            break;
                        }
                        Err(e) => {
                            info!("Error receiving message: {:?}", e);
                            break;
                        }
                        _ => {}
                    }
                }
            }
            Err(e) => {
                info!("Failed to connect: {:?}", e);
                sleep(Duration::from_secs(1)).await;
            }
        }
    }
}
