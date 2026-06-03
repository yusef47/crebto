use alloy::{
    providers::{Provider, ProviderBuilder, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::eth::{Filter, Log},
    primitives::{address, Address, B256},
};
use futures_util::StreamExt;
use tokio::sync::mpsc::Sender;
use tracing::{info, error};
use std::sync::Arc;

pub const UNISWAP_V3_SWAP_TOPIC: B256 = alloy::primitives::b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");

pub const AERODROME_V2_SWAP_TOPIC: B256 = alloy::primitives::b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d824");

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
                Ok(sub) => {
                    let mut stream = sub.into_stream();
                    while let Some(block) = stream.next().await {
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
                Ok(sub) => {
                    let mut stream = sub.into_stream();
                    while let Some(log) = stream.next().await {
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
