use alloy::primitives::Address;
use serde_json::json;
use tracing::{info, warn};

pub struct MevBundle {
    pub signed_txs: Vec<Vec<u8>>,
    pub block_target: u64,
    pub refund_address: Address,
    pub refund_percent: u64,
}

impl MevBundle {
    pub fn to_json(&self) -> serde_json::Value {
        json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "mev_sendBundle",
            "params": [{
                "version": "v2",
                "inclusion": {
                    "block": self.block_target,
                    "maxBlock": self.block_target + 2,
                },
                "body": self.signed_txs.iter().map(|tx| {
                    json!({"tx": format!("0x{}", tx.iter().map(|b| format!("{:02x}", b)).collect::<String>()), "canRevert": false})
                }).collect::<Vec<_>>(),
                "validity": {
                    "refund": [
                        {
                            "address": format!("{}", self.refund_address),
                            "percent": self.refund_percent
                        }
                    ]
                }
            }]
        })
    }
}

pub struct MevShareSubmitter;

impl MevShareSubmitter {
    pub async fn submit(
        bundle: &MevBundle,
        endpoint: &str,
    ) -> eyre::Result<()> {
        let client = reqwest::Client::new();
        let body = bundle.to_json();

        let resp = client
            .post(endpoint)
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await?;

        let status = resp.status();
        let text = resp.text().await?;

        if status.is_success() {
            info!("✅ Bundle submitted to MEV-Share for block {}: {}", bundle.block_target, text);
        } else {
            warn!("❌ Bundle rejected ({}): {}", status, text);
        }

        Ok(())
    }
}
