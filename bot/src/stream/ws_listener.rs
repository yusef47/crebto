use alloy::{
    providers::{Provider, ProviderBuilder, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::eth::{Filter, Log},
    primitives::{address, Address, B256},
};
use tokio::sync::mpsc::Sender;
use tracing::{info, error};
use std::sync::Arc;

pub const UNISWAP_V3_SWAP_TOPIC: B256 = B256::from_slice(&[
    0xc4, 0x20, 0x79, 0xf9, 0x4a, 0x63, 0x50, 0xd7, 0xe6, 0x23, 0x5f, 0x29, 0x17, 0x49, 0x24, 0xf9,
    0x28, 0xcc, 0x2a, 0xc8, 0x18, 0xeb, 0x64, 0xfe, 0xd8, 0x00, 0x4e, 0x11, 0x5f, 0xbc, 0xca, 0x67,
]);

pub const AERODROME_V2_SWAP_TOPIC: B256 = B256::from_slice(&[
    0xd7, 0x8a, 0xd9, 0x5f, 0xa4, 0x6c, 0x99, 0x4b, 0x65, 0x51, 0xd0, 0xda, 0x85, 0xfc, 0x27, 0x5f,
    0xe6, 0x13, 0xce, 0x37, 0x65, 0x7f, 0xb8, 0xd5, 0xe3, 0xd1, 0x30, 0x84, 0x01, 0x59, 0xd8, 0x24,
]);

pub struct WsListener {
    wss_url: String,
}

impl WsListener {
    pub fn new(wss_url: String) -> Self {
        Self { wss_url }
    }

    pub async fn listen(&self, log_tx: Sender<Log>, block_tx: Sender<u64>) -> Result<(), eyre::Report> {
        info!("Connecting to Alchemy WSS: {}", self.wss_url);
        let ws = WsConnect::new(&self.wss_url);
        let provider = ProviderBuilder::new().on_ws(ws).await?;
        let provider = Arc::new(provider);

        info!("Connected successfully. Subscribing to Swap logs & Blocks...");

        // Subscribe to Blocks (for Flashblocks or standard block updates)
        let block_provider = provider.clone();
        let block_tx_clone = block_tx.clone();
        tokio::spawn(async move {
            match block_provider.subscribe_blocks().await {
                Ok(mut sub) => {
                    while let Some(block) = sub.next().await {
                        let block_number = block.header.number.unwrap_or(0);
                        if block_number > 0 {
                            if let Err(e) = block_tx_clone.send(block_number).await {
                                error!("Failed to send block number: {:?}", e);
                                break;
                            }
                        }
                    }
                }
                Err(e) => {
                    error!("Block subscription error: {:?}", e);
                }
            }
        });

        // Subscribe to Swap Logs
        let filter = Filter::new()
            .event_signature(vec![
                UNISWAP_V3_SWAP_TOPIC,
                AERODROME_V2_SWAP_TOPIC,
            ]);

        let log_provider = provider.clone();
        let log_tx_clone = log_tx.clone();
        tokio::spawn(async move {
            match log_provider.subscribe_logs(&filter).await {
                Ok(mut sub) => {
                    while let Some(log) = sub.next().await {
                        if let Err(e) = log_tx_clone.send(log).await {
                            error!("Failed to send log: {:?}", e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("Logs subscription error: {:?}", e);
                }
            }
        });

        Ok(())
    }
}
