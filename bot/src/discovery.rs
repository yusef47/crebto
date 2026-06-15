use alloy::{
    primitives::{Address, U256},
    providers::Provider,
    pubsub::PubSubFrontend,
    sol,
};
use tracing::{info, warn};

use crate::{AERODROME_V2_FACTORY, AERODROME_V2_ROUTER, WETH, USDC, ZERO_ADDRESS};

sol! {
    #[sol(rpc)]
    interface IAerodromeRouter {
        function poolFor(address tokenA, address tokenB, bool stable, address factory) external view returns (address pool);
    }

    #[sol(rpc)]
    interface IAerodromeV2Pool {
        function token0() external view returns (address token);
        function token1() external view returns (address token);
        function getReserves() external view returns (uint256 reserve0, uint256 reserve1, uint32 blockTimestampLast);
        function stable() external view returns (bool);
    }

    #[sol(rpc)]
    interface IERC20 {
        function decimals() external view returns (uint8);
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
    /// Estimated liquidity in USD (using WETH price as reference)
    pub liquidity_usd: f64,
}

/// Dynamic pool scanner for Aerodrome V2.
/// Scans token pairs against WETH/USDC to find pools with sufficient liquidity.
pub struct PoolDiscovery;

impl PoolDiscovery {
    /// Scan a single token pair for its Aerodrome V2 pool.
    /// Returns the pool if it exists, has non-zero reserves, and meets liquidity threshold.
    pub async fn scan_pair<P: Provider<PubSubFrontend>>(
        provider: &P,
        token_a: Address,
        token_b: Address,
        stable: bool,
        fee_bps: u32,
        label: &str,
        weth_price_usd: f64,
        min_liquidity_usd: f64,
    ) -> Option<DiscoveredPool> {
        let router = IAerodromeRouter::new(AERODROME_V2_ROUTER, provider);

        let pool_addr = match router.poolFor(token_a, token_b, stable, AERODROME_V2_FACTORY).call().await {
            Ok(result) if result.pool != ZERO_ADDRESS => result.pool,
            _ => return None,
        };

        let pool = IAerodromeV2Pool::new(pool_addr, provider);

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

        let dec0 = Self::fetch_decimals(provider, token0).await;
        let dec1 = Self::fetch_decimals(provider, token1).await;

        let liquidity_usd = Self::estimate_liquidity_usd(
            reserves.reserve0, reserves.reserve1,
            token0, token1,
            dec0, dec1,
            weth_price_usd,
        );

        if liquidity_usd < min_liquidity_usd {
            info!("Pool {} for {} has ${:.2} liquidity (below ${:.2} threshold)",
                pool_addr, label, liquidity_usd, min_liquidity_usd);
            return None;
        }

        info!("🎯 Discovered pool {} for {}: ${:.2} liquidity", pool_addr, label, liquidity_usd);

        Some(DiscoveredPool {
            address: pool_addr,
            token0,
            token1,
            reserve0: reserves.reserve0,
            reserve1: reserves.reserve1,
            fee_bps,
            stable,
            label: label.to_string(),
            liquidity_usd,
        })
    }

    /// Scan a list of token pairs and return all pools that meet the liquidity threshold.
    pub async fn scan_pairs<P: Provider<PubSubFrontend>>(
        provider: &P,
        pairs: &[(Address, Address, bool, u32, &'static str)],
        weth_price_usd: f64,
        min_liquidity_usd: f64,
    ) -> Vec<DiscoveredPool> {
        let mut discovered = Vec::new();

        for (token_a, token_b, stable, fee_bps, label) in pairs {
            if let Some(pool) = Self::scan_pair(
                provider, *token_a, *token_b, *stable, *fee_bps,
                label, weth_price_usd, min_liquidity_usd,
            ).await {
                discovered.push(pool);
            }
        }

        info!("🔍 Discovery complete: {} pools passed liquidity filter", discovered.len());
        discovered
    }

    /// Scan a seed token against all known base tokens (WETH, USDC) to discover new pairs.
    /// For long-tail tokens, we only care about WETH and USDC pairs.
    pub async fn discover_long_tail_pairs<P: Provider<PubSubFrontend>>(
        provider: &P,
        seed_tokens: &[Address],
        weth_price_usd: f64,
        min_liquidity_usd: f64,
    ) -> Vec<DiscoveredPool> {
        let base_tokens = vec![WETH, USDC];
        let mut pairs = Vec::new();

        for token in seed_tokens {
            for base in &base_tokens {
                if token == base {
                    continue;
                }
                // Volatile pool against WETH/USDC
                pairs.push((*token, *base, false, 30, "volatile"));
            }
        }

        Self::scan_pairs(provider, &pairs, weth_price_usd, min_liquidity_usd).await
    }

    /// Estimate total liquidity in USD for a pool.
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

        if token0 == WETH {
            r0 * weth_price_usd * 2.0
        } else if token1 == WETH {
            r1 * weth_price_usd * 2.0
        } else if token0 == USDC {
            r0 * 2.0
        } else if token1 == USDC {
            r1 * 2.0
        } else {
            (r0 * weth_price_usd).max(r1 * weth_price_usd)
        }
    }

    async fn fetch_decimals<P: Provider<PubSubFrontend>>(provider: &P, token: Address) -> u32 {
        let contract = IERC20::new(token, provider);
        match contract.decimals().call().await {
            Ok(result) => result._0 as u32,
            Err(_) => {
                warn!("Failed to fetch decimals for {:?}, defaulting to 18", token);
                18
            }
        }
    }
}
