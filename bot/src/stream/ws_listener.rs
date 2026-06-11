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

pub const AERODROME_V2_SWAP_TOPIC: B256 = alloy::primitives::b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

pub const UNISWAP_V3_FACTORY: Address = address!("33128a8fC17869897dcE68Ed026d694621f6FDfD");
pub const AERODROME_V2_FACTORY: Address = address!("420DD381b31aEf6683db6B902084cB0FFECe40Da");
pub const AERODROME_SLIPSTREAM_FACTORY: Address = address!("5e7BB104d84c7CB9B682AaC2F3d509f5F406809A");

pub struct WsListener {
    wss_url: String,
}

impl WsListener {
    pub fn new(wss_url: String) -> Self {
        Self { wss_url }
    }

    pub async fn listen(&self, log_tx: Sender<Log>, block_tx: Sender<(u64, u64)>, addresses: Vec<Address>) -> Result<(), eyre::Report> {
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
                        let block_number = block.header.number;
                        let base_fee = block.header.base_fee_per_gas
                            .and_then(|v| u64::try_from(v).ok())
                            .unwrap_or(50_000_000);
                        if block_number > 0 {
                            if let Err(e) = block_tx_clone.send((block_number, base_fee)).await {
                                error!("Failed to send block info: {:?}", e);
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

        // Subscribe to Swap Logs (filtered by our tracked pool addresses)
        let filter = Filter::new()
            .address(addresses)
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

        // Subscribe to Factory PoolCreated events for dynamic pool discovery.
        let factory_filter = Filter::new()
            .address(vec![
                UNISWAP_V3_FACTORY,
                AERODROME_V2_FACTORY,
                AERODROME_SLIPSTREAM_FACTORY,
            ])
            .event_signature(vec![
                crate::stream::new_pair_watcher::UNISWAP_V3_POOL_CREATED_TOPIC,
                crate::stream::new_pair_watcher::AERODROME_POOL_CREATED_TOPIC,
                crate::stream::new_pair_watcher::AERODROME_SLIPSTREAM_POOL_CREATED_TOPIC,
            ]);

        let factory_provider = provider.clone();
        let factory_tx_clone = log_tx.clone();
        tokio::spawn(async move {
            match factory_provider.subscribe_logs(&factory_filter).await {
                Ok(sub) => {
                    let mut stream = sub.into_stream();
                    while let Some(log) = stream.next().await {
                        if let Err(e) = factory_tx_clone.send(log).await {
                            error!("Failed to send factory log: {:?}", e);
                            break;
                        }
                    }
                }
                Err(e) => {
                    error!("Factory logs subscription error: {:?}", e);
                }
            }
        });

        Ok(())
    }
}
