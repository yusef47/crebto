use alloy::{
    network::Ethereum,
    primitives::{Address, Bytes, U256},
    providers::Provider,
    rpc::types::eth::TransactionRequest,
    sol,
    transports::Transport,
};
use tracing::{info, warn};

use crate::{config::Config, WSEI, USDC};
/// Retry a raw eth_call up to `max_retries` times with fixed 2-second sleep.
/// Returns Ok(bytes) on success, Err(last_error) after exhaustion.
async fn retry_eth_call<T, P>(
    provider: &P,
    tx: &alloy::rpc::types::eth::TransactionRequest,
    max_retries: usize,
) -> Result<alloy::primitives::Bytes, Box<dyn std::error::Error + Send + Sync>>
where
    P: Provider<T, Ethereum>,
    T: Transport + Clone,
{
    let mut last_err: Option<Box<dyn std::error::Error + Send + Sync>> = None;
    for attempt in 0..max_retries {
        match provider.call(tx).await {
            Ok(bytes) => return Ok(bytes),
            Err(e) => {
                let msg = format!("{}", e);
                if msg.contains("-32000") || msg.to_lowercase().contains("rate limit") || msg.to_lowercase().contains("server busy") {
                    warn!("RPC rate limit (attempt {}/{}), retrying in 2s...", attempt + 1, max_retries);
                    tokio::time::sleep(tokio::time::Duration::from_secs(2)).await;
                    last_err = Some(Box::new(e));
                } else {
                    return Err(Box::new(e));
                }
            }
        }
    }
    Err(last_err.unwrap_or_else(|| "retry exhausted".into()))
}



sol! {
    #[sol(rpc)]
    interface IV2Pool {
        function token0() external view returns (address token);
        function token1() external view returns (address token);
        function getReserves() external view returns (uint256 reserve0, uint256 reserve1, uint32 blockTimestampLast);
    }

    #[sol(rpc)]
    interface IERC20 {
        function decimals() external view returns (uint8 dec);
    }
}

/// Discovered pool candidate with metadata for safety evaluation.
#[derive(Debug, Clone)]
pub struct DiscoveredPool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_bps: u32,
    pub stable: bool,
    pub label: String,
    /// Estimated liquidity in USD (using WSEI price as reference)
    pub liquidity_usd: f64,
}

/// Factory metadata for scanning.
#[derive(Debug, Clone)]
pub struct FactoryMeta {
    pub address: Address,
    pub dex_name: &'static str,
    pub fee_bps: u32,
}

/// Quiet-pool scanner for Sei EVM long-tail strategy.
/// Targets $5k–$30k liquidity pools and skips major contested pairs.
pub struct FactoryScanner {
    factories: Vec<FactoryMeta>,
    base_tokens: Vec<Address>,
    excluded_pairs: Vec<(Address, Address)>,
    pub min_liquidity_usd: f64,
    pub max_liquidity_usd: f64,
    /// How many recent pairs to scan per factory on each hourly pass.
    pub scan_window: usize,
}

impl FactoryScanner {
    pub fn from_config(config: &Config) -> Self {
        let mut factories = Vec::new();

        if let Some(addr) = config.saphyre_factory {
            factories.push(FactoryMeta { address: addr, dex_name: "saphyre", fee_bps: 30 });
        }
        if let Some(addr) = config.dragonswap_factory {
            factories.push(FactoryMeta { address: addr, dex_name: "dragonswap", fee_bps: 30 });
        }

        let base_tokens = vec![WSEI, USDC];

        let mut excluded_pairs = Vec::new();
        excluded_pairs.push(sort_pair(WSEI, USDC));

        Self {
            factories,
            base_tokens,
            excluded_pairs,
            min_liquidity_usd: config.min_liquidity_usd,
            max_liquidity_usd: config.max_liquidity_usd,
            scan_window: 50,
        }
    }

    pub fn add_factory(&mut self, address: Address, dex_name: &'static str, fee_bps: u32) {
        self.factories.push(FactoryMeta { address, dex_name, fee_bps });
    }

    /// Scan the most recent `scan_window` pairs from every configured factory.
    pub async fn scan_recent_pairs<T, P>(
        &self,
        provider: &P,
        weth_price_usd: f64,
    ) -> Vec<DiscoveredPool>
    where
        P: Provider<T, Ethereum>,
        T: Transport + Clone,
    {
        let mut discovered = Vec::new();

        for factory_meta in &self.factories {
            // Raw eth_call for allPairsLength — bypass alloy sol! decoding issues
            let tx_len = TransactionRequest::default()
                .to(factory_meta.address)
                .input(Bytes::from_static(&[0x57, 0x4f, 0x2b, 0xa3]).into());
            let total_pairs = match retry_eth_call(provider, &tx_len, 5).await {
                Ok(bytes) => {
                    if bytes.len() < 32 {
                        warn!("Factory {} returned {} bytes for allPairsLength, expected 32", factory_meta.dex_name, bytes.len());
                        continue;
                    }
                    U256::from_be_slice(&bytes).to::<u64>() as usize
                }
                Err(e) => {
                    warn!("Failed to get allPairsLength for {} factory (raw call): {}", factory_meta.dex_name, e);
                    continue;
                }
            };

            if total_pairs == 0 {
                continue;
            }

            let start = total_pairs.saturating_sub(self.scan_window);
            info!(
                "🔍 Scanning {} factory [{}] — pairs {}..{} of {}",
                factory_meta.dex_name, factory_meta.address, start, total_pairs, total_pairs
            );

            for i in start..total_pairs {
                let mut call_data = vec![0x1e, 0x3d, 0xd1, 0x8b];
                call_data.extend_from_slice(&U256::from(i).to_be_bytes_vec());
                let tx_pair = TransactionRequest::default()
                    .to(factory_meta.address)
                    .input(Bytes::from(call_data).into());
                let pool_addr = match retry_eth_call(provider, &tx_pair, 5).await {
                    Ok(bytes) => {
                        if bytes.len() < 32 {
                            continue;
                        }
                        Address::from_slice(&bytes[12..32])
                    }
                    Err(_) => continue,
                };

                if let Some(pool) = self.evaluate_pool(provider, pool_addr, factory_meta, weth_price_usd).await {
                    discovered.push(pool);
                }
            }
        }

        info!("🔍 Quiet-pool scan complete: {} pools in $5k–$30k range", discovered.len());
        discovered
    }

    async fn evaluate_pool<T, P>(
        &self,
        provider: &P,
        pool_addr: Address,
        meta: &FactoryMeta,
        weth_price_usd: f64,
    ) -> Option<DiscoveredPool>
    where
        P: Provider<T, Ethereum>,
        T: Transport + Clone,
    {
        let pool = IV2Pool::new(pool_addr, provider);

        let token0 = match pool.token0().call().await {
            Ok(result) => result.token,
            Err(_) => return None,
        };
        let token1 = match pool.token1().call().await {
            Ok(result) => result.token,
            Err(_) => return None,
        };

        let reserves = match pool.getReserves().call().await {
            Ok(result) => result,
            Err(_) => return None,
        };

        if reserves.reserve0.is_zero() || reserves.reserve1.is_zero() {
            return None;
        }

        if self.is_excluded_pair(token0, token1) {
            return None;
        }

        let dec0 = Self::fetch_decimals(provider, token0).await;
        let dec1 = Self::fetch_decimals(provider, token1).await;

        let liquidity_usd = Self::estimate_liquidity_usd(
            reserves.reserve0, reserves.reserve1,
            token0, token1,
            dec0, dec1,
            weth_price_usd,
        );

        if liquidity_usd < self.min_liquidity_usd || liquidity_usd > self.max_liquidity_usd {
            return None;
        }

        info!("🎯 Quiet pool discovered: {} ({:?}/{:?}) — ${:.2} liquidity",
            pool_addr, token0, token1, liquidity_usd);

        Some(DiscoveredPool {
            address: pool_addr,
            token0,
            token1,
            reserve0: reserves.reserve0,
            reserve1: reserves.reserve1,
            fee_bps: meta.fee_bps,
            stable: false,
            label: format!("{}-{}-{}", meta.dex_name, token0, token1),
            liquidity_usd,
        })
    }

    /// Backwards-compatible wrapper; delegates to `scan_recent_pairs`.
    pub async fn discover_long_tail_pairs<T, P>(
        &self,
        provider: &P,
        _seed_tokens: &[Address],
        weth_price_usd: f64,
    ) -> Vec<DiscoveredPool>
    where
        P: Provider<T, Ethereum>,
        T: Transport + Clone,
    {
        self.scan_recent_pairs(provider, weth_price_usd).await
    }

    fn is_excluded_pair(&self, token0: Address, token1: Address) -> bool {
        let key = sort_pair(token0, token1);
        self.excluded_pairs.iter().any(|ex| sort_pair(ex.0, ex.1) == key)
    }

    fn estimate_liquidity_usd(
        reserve0: U256,
        reserve1: U256,
        token0: Address,
        token1: Address,
        dec0: u32,
        dec1: u32,
        weth_price_usd: f64,
    ) -> f64 {
        let r0 = reserve0.to::<u128>() as f64 / 10_f64.powi(dec0 as i32);
        let r1 = reserve1.to::<u128>() as f64 / 10_f64.powi(dec1 as i32);

        if token0 == WSEI {
            r0 * weth_price_usd * 2.0
        } else if token1 == WSEI {
            r1 * weth_price_usd * 2.0
        } else if token0 == USDC {
            r0 * 2.0
        } else if token1 == USDC {
            r1 * 2.0
        } else {
            (r1 * weth_price_usd + r0 * weth_price_usd) / 2.0
        }
    }

    async fn fetch_decimals<T, P>(provider: &P, token: Address) -> u32
    where
        P: Provider<T, Ethereum>,
        T: Transport + Clone,
    {
        let contract = IERC20::new(token, provider);
        match contract.decimals().call().await {
            Ok(result) => result.dec as u32,
            Err(_) => {
                warn!("Failed to fetch decimals for {:?}, defaulting to 18", token);
                18
            }
        }
    }

    /// Spawn a background task that rescans factories every hour.
    pub fn spawn_hourly_scan<T, P>(
        self,
        provider: std::sync::Arc<P>,
        weth_price_usd: std::sync::Arc<tokio::sync::RwLock<f64>>,
        tx: tokio::sync::mpsc::Sender<DiscoveredPool>,
    ) -> tokio::task::JoinHandle<()>
    where
        P: Provider<T, Ethereum> + 'static,
        T: Transport + Clone + 'static,
    {
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(3600));
            loop {
                interval.tick().await;
                let price = *weth_price_usd.read().await;
                let pools = self.scan_recent_pairs(provider.as_ref(), price).await;
                for pool in pools {
                    if tx.send(pool).await.is_err() {
                        warn!("Hourly scan receiver dropped; stopping scanner task.");
                        return;
                    }
                }
            }
        })
    }
}

fn sort_pair(a: Address, b: Address) -> (Address, Address) {
    if a < b { (a, b) } else { (b, a) }
}
