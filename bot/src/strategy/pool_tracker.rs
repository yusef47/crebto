use alloy::primitives::{Address, U256};
use crate::dex::traits::PoolState;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::atomic::{AtomicU64, Ordering};
use tracing::info;

/// Tracks pool states in memory, updated live from swap events.
/// Groups pools by token pair so we can find cross-DEX arbitrage.
#[derive(Debug, Clone)]
pub struct TrackedPool {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub sqrt_price_x96: U256,
    pub liquidity: u128,
    pub tick: i32,
    pub fee_bps: u32,
    pub tick_spacing: Option<i32>,
    pub dex_name: String, // "uniswap_v3" or "aerodrome_cl"
    pub last_update_block: u64,
}

/// Represents a detected arbitrage opportunity
#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    pub pool_a: Address,
    pub pool_b: Address,
    pub dex_a: String,
    pub dex_b: String,
    pub token0: Address,
    pub token1: Address,
    pub price_a: f64,
    pub price_b: f64,
    pub spread_bps: f64,
    pub estimated_profit_usd: f64,
    pub gas_cost_usd: f64,
}

#[derive(Debug, Clone, Eq)]
struct OpportunityKey {
    block: u64,
    pool_a: Address,
    pool_b: Address,
    price_a_bits: u64,
    price_b_bits: u64,
}

impl PartialEq for OpportunityKey {
    fn eq(&self, other: &Self) -> bool {
        self.block == other.block
            && self.pool_a == other.pool_a
            && self.pool_b == other.pool_b
            && self.price_a_bits == other.price_a_bits
            && self.price_b_bits == other.price_b_bits
    }
}

impl Hash for OpportunityKey {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.block.hash(state);
        self.pool_a.hash(state);
        self.pool_b.hash(state);
        self.price_a_bits.hash(state);
        self.price_b_bits.hash(state);
    }
}

/// Normalizes a token pair key (always smaller address first)
fn pair_key(token0: Address, token1: Address) -> (Address, Address) {
    if token0 < token1 {
        (token0, token1)
    } else {
        (token1, token0)
    }
}


/// Converts sqrtPriceX96 to a floating point price ratio
fn sqrt_price_to_f64(sqrt_price_x96: U256) -> f64 {
    // sqrtPriceX96 = sqrt(price) * 2^96
    // price = (sqrtPriceX96 / 2^96)^2
    let q96: f64 = (2.0_f64).powi(96);
    
    // Convert U256 to f64 safely (may lose precision for very large values)
    let sqrt_val = u256_to_f64(sqrt_price_x96);
    let ratio = sqrt_val / q96;
    ratio * ratio
}

fn u256_to_f64(v: U256) -> f64 {
    let limbs = v.as_limbs();
    let mut result: f64 = 0.0;
    let base: f64 = (2.0_f64).powi(64);
    for i in (0..4).rev() {
        result = result * base + (limbs[i] as f64);
    }
    result
}

fn fee_to_bps(fee: u32, dex_name: &str) -> f64 {
    if dex_name == "aerodrome_v2" {
        fee as f64
    } else {
        fee as f64 / 100.0
    }
}

fn infer_tick_spacing(fee: u32, dex_name: &str) -> Option<i32> {
    if dex_name != "aerodrome_cl" {
        return None;
    }

    match fee {
        100 => Some(1),
        500 => Some(100),
        3000 => Some(200),
        10000 => Some(2000),
        _ => Some(fee as i32),
    }
}

pub struct PoolTracker {
    /// Map from pool address -> TrackedPool
    pools: HashMap<Address, TrackedPool>,
    /// Map from (token0, token1) pair -> Vec of pool addresses
    pair_to_pools: HashMap<(Address, Address), Vec<Address>>,
    /// Known pool address -> (token0, token1, fee, tick_spacing, dex_name)
    known_pools: HashMap<Address, (Address, Address, u32, Option<i32>, String)>,
    /// Current gas price in wei (updated each block)
    current_gas_price_wei: u64,
    /// Estimated gas units for a 2-hop flash arb tx on Base
    estimated_gas_units: u64,
    /// Cache of token decimals for dynamic arbitrage calculations
    pub decimals_cache: HashMap<Address, u32>,
    /// Blocks duplicate opportunities caused by clustered logs in the same block.
    seen_opportunities: HashSet<OpportunityKey>,
    last_cache_cleanup_block: u64,
    min_profit_usd: f64,
}

impl PoolTracker {
    pub fn new() -> Self {
        let mut tracker = Self {
            pools: HashMap::new(),
            pair_to_pools: HashMap::new(),
            known_pools: HashMap::new(),
            current_gas_price_wei: 50_000_000, // 0.05 gwei default (Base L2 typical)
            estimated_gas_units: 350_000, // ~350k gas for a flash arb tx
            decimals_cache: HashMap::new(),
            seen_opportunities: HashSet::new(),
            last_cache_cleanup_block: 0,
            min_profit_usd: 1.0,
        };
        tracker.load_known_pools();
        tracker
    }

    /// Pre-loads known high-volume pool pairs on Base for cross-DEX comparison.
    /// These are pools where the SAME token pair exists on multiple DEXes.
    fn load_known_pools(&mut self) {
        // Common Base tokens
        let weth: Address = "0x4200000000000000000000000000000000000006".parse().unwrap();
        let usdc: Address = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".parse().unwrap();
        let cbbtc: Address = "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf".parse().unwrap();
        let usdbc: Address = "0xd9aAEc86B65D86f6A7B5B1b0c42FFA531710b6CA".parse().unwrap();
        let dai: Address = "0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb".parse().unwrap();

        self.decimals_cache.insert(weth, 18);
        self.decimals_cache.insert(usdc, 6);
        self.decimals_cache.insert(cbbtc, 8);
        self.decimals_cache.insert(usdbc, 6);
        self.decimals_cache.insert(dai, 18);

        // === WETH/USDC pools ===
        // Uniswap V3 WETH/USDC 0.05%
        self.register_pool("0xd0b53d9277642d899df5c87a3966a349a798f224".parse().unwrap(),
            weth, usdc, 500, "uniswap_v3");
        // Uniswap V3 WETH/USDC 0.3%
        self.register_pool("0xb4cb800910b228ed3d0834cf79d697127bbb00e5".parse().unwrap(),
            weth, usdc, 3000, "uniswap_v3");
        // Aerodrome CL WETH/USDC (Slipstream)
        self.register_pool("0xb2cc224c1c9fee385f8ad6a55b4d94e92359dc59".parse().unwrap(),
            weth, usdc, 100, "aerodrome_cl");
        // Aerodrome CL WETH/USDC (another tick spacing)
        self.register_pool("0xdbc6998296caa1652a810dc8d3baf4a8294330f1".parse().unwrap(),
            weth, usdc, 500, "aerodrome_cl");
        // Aerodrome CL WETH/USDC
        self.register_pool("0x4e392fbfe4d0557c82d2f97f02ec39daa31516dd".parse().unwrap(),
            weth, usdc, 100, "aerodrome_cl");

        // === cbBTC/USDC pools ===
        // Aerodrome CL cbBTC/USDC
        self.register_pool("0x4e962bb3889bf030368f56810a9c96b83cb3e778".parse().unwrap(),
            cbbtc, usdc, 500, "aerodrome_cl");
        // Uniswap V3 cbBTC/USDC (if exists)
        
        // === WETH/cbBTC pools ===
        self.register_pool("0x70acdf2ad0bf2402c957154f944c19ef4e1cbae1".parse().unwrap(),
            weth, cbbtc, 3000, "uniswap_v3");
        self.register_pool("0x42d4a22cad0f5a49681a5715ce994af73a43b76b".parse().unwrap(),
            weth, cbbtc, 500, "aerodrome_cl");

        info!("📋 Pool Registry loaded: {} known pools across {} pairs",
            self.known_pools.len(),
            self.pair_to_pools.len()
        );
    }

    pub fn register_pool(&mut self, address: Address, token0: Address, token1: Address, fee: u32, dex: &str) {
        let tick_spacing = infer_tick_spacing(fee, dex);
        self.register_pool_with_tick_spacing(address, token0, token1, fee, tick_spacing, dex);
    }

    pub fn register_pool_with_tick_spacing(
        &mut self,
        address: Address,
        token0: Address,
        token1: Address,
        fee: u32,
        tick_spacing: Option<i32>,
        dex: &str,
    ) {
        let key = pair_key(token0, token1);
        self.known_pools
            .insert(address, (token0, token1, fee, tick_spacing, dex.to_string()));
        let pools = self.pair_to_pools.entry(key).or_default();
        if !pools.contains(&address) {
            pools.push(address);
        }
    }

    pub fn get_pool_dex(&self, pool_address: &Address) -> Option<String> {
        self.known_pools.get(pool_address).map(|(_, _, _, _, dex)| dex.clone())
    }

    pub fn get_pool_state(&self, pool_address: &Address) -> Option<PoolState> {
        let pool = self.pools.get(pool_address)?;
        Some(PoolState {
            address: pool.address,
            token0: pool.token0,
            token1: pool.token1,
            reserve0: U256::ZERO,
            reserve1: U256::ZERO,
            fee_bps: pool.fee_bps,
            tick_spacing: pool.tick_spacing,
            sqrt_price_x96: pool.sqrt_price_x96,
            liquidity: pool.liquidity,
            current_tick: pool.tick,
        })
    }

    pub fn register_token_decimals(&mut self, token: Address, decimals: u32) {
        self.decimals_cache.insert(token, decimals);
    }

    pub fn get_token_decimals(&self, token: Address) -> u32 {
        if let Some(&decimals) = self.decimals_cache.get(&token) {
            return decimals;
        }

        let weth: Address = "0x4200000000000000000000000000000000000006".parse().unwrap();
        let usdc: Address = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".parse().unwrap();
        let cbbtc: Address = "0xcbB7C0000aB88B473b1f5aFd9ef808440eed33Bf".parse().unwrap();
        let usdbc: Address = "0xd9aAEc86B65D86f6A7B5B1b0c42FFA531710b6CA".parse().unwrap();
        let dai: Address = "0x50c5725949A6F0c72E6C4a641F24049A917DB0Cb".parse().unwrap();

        if token == weth {
            18
        } else if token == usdc {
            6
        } else if token == cbbtc {
            8
        } else if token == usdbc {
            6
        } else if token == dai {
            18
        } else {
            18 // Default fallback
        }
    }

    /// Updates the current gas price from the latest block.
    pub fn update_gas_price(&mut self, gas_price_wei: u64) {
        self.current_gas_price_wei = gas_price_wei;
    }

    pub fn set_min_profit_usd(&mut self, min_profit_usd: f64) {
        self.min_profit_usd = min_profit_usd.max(0.0);
    }

    /// Estimates the gas cost of executing a flash arb in USD.
    /// Uses ETH price derived from WETH/USDC pool data if available.
    fn estimate_gas_cost_usd(&self) -> f64 {
        // Gas cost in ETH = gas_price * gas_units / 1e18
        let gas_cost_eth = (self.current_gas_price_wei as f64) * (self.estimated_gas_units as f64) / 1e18;

        // Get ETH price from our tracked WETH/USDC pools
        let eth_price_usd = self.get_eth_price_usd().unwrap_or(2500.0);

        gas_cost_eth * eth_price_usd
    }

    /// Derives ETH/USD price from any tracked WETH/USDC pool.
    pub fn get_eth_price_usd(&self) -> Option<f64> {
        let weth: Address = "0x4200000000000000000000000000000000000006".parse().ok()?;
        let usdc: Address = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".parse().ok()?;
        let key = pair_key(weth, usdc);

        let pool_addrs = self.pair_to_pools.get(&key)?;
        for addr in pool_addrs {
            if let Some(pool) = self.pools.get(addr) {
                if !pool.sqrt_price_x96.is_zero() {
                    let raw_price = sqrt_price_to_f64(pool.sqrt_price_x96);
                    // WETH is token0 in most Base pools, USDC is token1
                    // price = token1/token0 = USDC per WETH
                    // But USDC has 6 decimals, WETH has 18
                    // So real_price = raw_price * 10^(18-6) = raw_price * 10^12
                    if pool.token0 == weth {
                        return Some(raw_price * 1e12);
                    } else {
                        if raw_price > 0.0 {
                            return Some((1.0 / raw_price) * 1e12);
                        }
                    }
                }
            }
        }
        None
    }

    /// Updates a pool's price state from a decoded V3-style swap event.
    /// Returns any detected arbitrage opportunities for this pair.
    pub fn update_from_swap(
        &mut self,
        pool_address: Address,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
        current_block: u64,
    ) -> Vec<ArbOpportunity> {
        // Look up this pool in our registry
        let (token0, token1, fee, tick_spacing, dex_name) = match self.known_pools.get(&pool_address) {
            Some(info) => info.clone(),
            None => return vec![], // Unknown pool, skip
        };

        // Update or insert the tracked pool state
        let tracked = TrackedPool {
            address: pool_address,
            token0,
            token1,
            sqrt_price_x96,
            liquidity,
            tick,
            fee_bps: fee,
            tick_spacing,
            dex_name: dex_name.clone(),
            last_update_block: current_block,
        };
        self.pools.insert(pool_address, tracked);

        // Check for cross-DEX arbitrage on this pair
        self.check_arbitrage(token0, token1, current_block)
    }

    /// Compares prices across all pools for a given token pair.
    fn check_arbitrage(&mut self, token0: Address, token1: Address, current_block: u64) -> Vec<ArbOpportunity> {
        let key = pair_key(token0, token1);
        let pool_addrs = match self.pair_to_pools.get(&key) {
            Some(addrs) => addrs.clone(),
            None => return vec![],
        };

        if current_block > self.last_cache_cleanup_block + 8 {
            self.seen_opportunities
                .retain(|seen| seen.block + 8 >= current_block);
            self.last_cache_cleanup_block = current_block;
        }

        // Collect all pools that have been updated (have price data)
        let active_pools: Vec<TrackedPool> = pool_addrs.iter()
            .filter_map(|addr| self.pools.get(addr))
            .filter(|p| !p.sqrt_price_x96.is_zero())
            .cloned()
            .collect();

        if active_pools.len() < 2 {
            return vec![];
        }

        let mut opportunities = vec![];

        // Compare every pair of pools
        for i in 0..active_pools.len() {
            for j in (i + 1)..active_pools.len() {
                let pa = &active_pools[i];
                let pb = &active_pools[j];

                let price_a = sqrt_price_to_f64(pa.sqrt_price_x96);
                let price_b = sqrt_price_to_f64(pb.sqrt_price_x96);

                if price_a <= 0.0 || price_b <= 0.0 {
                    continue;
                }

                // Calculate spread in basis points
                let higher = price_a.max(price_b);
                let lower = price_a.min(price_b);
                let spread_bps = ((higher - lower) / lower) * 10000.0;

                // Combined fees from both pools (in bps)
                let total_fee_bps = fee_to_bps(pa.fee_bps, &pa.dex_name)
                    + fee_to_bps(pb.fee_bps, &pb.dex_name);

                // Dynamic gas cost estimation
                let gas_cost_usd = self.estimate_gas_cost_usd();

                // Minimum profitable spread = fees + gas as percentage of trade
                let trade_size_usd = 1000.0;
                let gas_bps = (gas_cost_usd / trade_size_usd) * 10000.0;
                let min_spread = total_fee_bps + gas_bps;

                if spread_bps > min_spread {
                    let profit_pct = (spread_bps - min_spread) / 10000.0;
                    let estimated_profit = trade_size_usd * profit_pct;

                    if estimated_profit < self.min_profit_usd {
                        continue;
                    }

                    let (key_pool_a, key_pool_b, key_price_a, key_price_b) = if pa.address <= pb.address {
                        (pa.address, pb.address, price_a.to_bits(), price_b.to_bits())
                    } else {
                        (pb.address, pa.address, price_b.to_bits(), price_a.to_bits())
                    };
                    let opportunity_key = OpportunityKey {
                        block: current_block,
                        pool_a: key_pool_a,
                        pool_b: key_pool_b,
                        price_a_bits: key_price_a,
                        price_b_bits: key_price_b,
                    };

                    if !self.seen_opportunities.insert(opportunity_key) {
                        continue;
                    }

                    // Convert raw price ratios to human-readable format based on token decimals
                    let dec0 = self.get_token_decimals(token0);
                    let dec1 = self.get_token_decimals(token1);
                    let price_multiplier = 10.0_f64.powi(dec0 as i32 - dec1 as i32);
                    let human_price_a = price_a * price_multiplier;
                    let human_price_b = price_b * price_multiplier;

                    opportunities.push(ArbOpportunity {
                        pool_a: pa.address,
                        pool_b: pb.address,
                        dex_a: pa.dex_name.clone(),
                        dex_b: pb.dex_name.clone(),
                        token0,
                        token1,
                        price_a: human_price_a,
                        price_b: human_price_b,
                        spread_bps,
                        estimated_profit_usd: estimated_profit,
                        gas_cost_usd,
                    });
                }
            }
        }

        opportunities
    }

    /// Returns current number of tracked pools with active price data
    pub fn active_pool_count(&self) -> usize {
        self.pools.len()
    }

    /// Returns a list of all pool addresses registered in the known pools mapping
    pub fn get_known_addresses(&self) -> Vec<Address> {
        self.known_pools.keys().cloned().collect()
    }
}
