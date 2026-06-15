// Production-ready, high-performance, and self-contained Crebto Arbitrage Bot
// Built for Base Layer-2 Network (2026)
// v0.6: Dynamic Shadow Sniper — auto-discovers long-tail/meme pools
//        with honeypot/tax safety filters and live trading config.

use alloy::{
    primitives::{address, Address, B256, U256},
    providers::{Provider, ProviderBuilder, WsConnect},
    pubsub::PubSubFrontend,
    rpc::types::eth::Filter,
    sol,
};
use futures_util::StreamExt;
use tracing::{info, warn, Level};
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;
use std::collections::HashMap;
use tokio::sync::RwLock;

mod config;
use config::Config;

mod discovery;
mod safety;
use discovery::PoolDiscovery;
use safety::{TokenSafetyChecker, TokenSafety};

// --- GLOBAL SETTINGS ---
// These are now loaded from Config. Fallbacks removed.
// const DRY_RUN: bool = true;
// const MIN_PROFIT_USD: f64 = 1.0;
// const GAS_LIMIT: u64 = 600_000;
// const BASE_FEE_WEI: u64 = 5_000_000; // ~0.005 gwei typical Base gas

pub const CHAINLINK_ETH_USD: Address = address!("71041dddad356df2e01399310d6b3f67c3071b60");

// --- VERIFIED EVENT SIGNATURES (KECCAK-256) ---
pub const UNISWAP_V3_SWAP_TOPIC: B256 = alloy::primitives::b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");
pub const AERODROME_V2_SWAP_TOPIC: B256 = alloy::primitives::b256!("b3e2773606abfd36b5bd91394b3a54d1398336c65005baf7bf7a05efeffaf75b");
pub const AERODROME_V2_SYNC_TOPIC: B256 = alloy::primitives::b256!("cf2aa50876cdfbb541206f89af0ee78d44a2abf8d328e37fa4917f982149848a");

pub const UNISWAP_V3_POOL_CREATED_TOPIC: B256 = alloy::primitives::b256!("783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118");
pub const AERODROME_POOL_CREATED_TOPIC: B256 = alloy::primitives::b256!("2128d88d14c80cb081c1252a5acff7a264671bf199ce226b53788fb26065005e");
pub const AERODROME_SLIPSTREAM_POOL_CREATED_TOPIC: B256 = alloy::primitives::b256!("ab0d57f0df537bb25e80245ef7748fa62353808c54d6e528a9dd20887aed9ac2");

pub const UNISWAP_V3_FACTORY: Address = address!("33128a8fC17869897dcE68Ed026d694621f6FDfD");
pub const AERODROME_V2_FACTORY: Address = address!("420DD381b31aEf6683db6B902084cB0FFECe40Da");
pub const AERODROME_V2_ROUTER: Address = address!("cF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43");
pub const AERODROME_SLIPSTREAM_FACTORY: Address = address!("5e7BB104d84c7CB9B682AaC2F3d509f5F406809A");
pub const ZERO_ADDRESS: Address = address!("0000000000000000000000000000000000000000");

// --- TARGET ASSETS ADDRESSES & DECIMALS ---
pub const WETH: Address = address!("4200000000000000000000000000000000000006");
pub const USDC: Address = address!("833589fCD6eDb6E08f4c7C32D4f71b54bdA02913");
pub const AERO: Address = address!("940181a94A35A4569E4529A3CDfB74e38FD98631");
pub const BRETT: Address = address!("532f27101965dd16442e59d40670faf5ebb142e4");
pub const DEGEN: Address = address!("4ed4E862860beD51a9570b96d89aF5E1B0Efefed");
pub const TOSHI: Address = address!("8544fe9d190fd7ec52860abbf45088e81ee24a8c");
pub const MIGGLES: Address = address!("B1a03EdA10342529bBF8EB700a06C60441fEf25d");

sol! {
    #[sol(rpc)]
    interface IERC20 {
        function decimals() external view returns (uint8);
        function symbol() external view returns (string);
    }

    #[sol(rpc)]
    interface IChainlinkPriceFeed {
        function latestAnswer() external view returns (int256 answer);
    }

    #[sol(rpc)]
    interface IAerodromeRouter {
        function poolFor(address tokenA, address tokenB, bool stable, address factory) external view returns (address pool);
    }

    #[sol(rpc)]
    interface IAerodromeV2Pool {
        function token0() external view returns (address token);
        function token1() external view returns (address token);
        function getReserves() external view returns (uint256 reserve0, uint256 reserve1, uint32 blockTimestampLast);
    }
}

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
    pub dex_name: String, // "uniswap_v3" or "aerodrome_cl" or "aerodrome_v2"
    pub last_update_block: u64,
    // V2 AMM reserve tracking
    pub reserve0: U256,
    pub reserve1: U256,
    // True for Aerodrome V2 stable pools (curve-based, not constant-product)
    pub stable: bool,
}

#[derive(Debug, Clone)]
pub struct ArbOpportunity {
    pub pool_a: Address,
    pub pool_b: Address,
    pub dex_a: String,
    pub dex_b: String,
    pub token_in: Address,
    pub token_out: Address,
    pub spread_bps: f64,
}

pub struct BotStats {
    pub swaps_detected: AtomicU64,
    pub opportunities_found: AtomicU64,
    pub profitable_after_fees: AtomicU64,
    pub uniswap_swaps: AtomicU64,
    pub aerodrome_cl_swaps: AtomicU64,
    pub aerodrome_v2_swaps: AtomicU64,
    pub blocks_seen: AtomicU64,
    pub consecutive_failures: AtomicU32,
    pub tracked_pools_active: AtomicU32,
    pub total_estimated_profit_cents: AtomicU64,
}

impl BotStats {
    fn new() -> Self {
        Self {
            swaps_detected: AtomicU64::new(0),
            opportunities_found: AtomicU64::new(0),
            profitable_after_fees: AtomicU64::new(0),
            uniswap_swaps: AtomicU64::new(0),
            aerodrome_cl_swaps: AtomicU64::new(0),
            aerodrome_v2_swaps: AtomicU64::new(0),
            blocks_seen: AtomicU64::new(0),
            consecutive_failures: AtomicU32::new(0),
            tracked_pools_active: AtomicU32::new(0),
            total_estimated_profit_cents: AtomicU64::new(0),
        }
    }

    fn print_report(&self, elapsed_secs: u64) {
        let swaps = self.swaps_detected.load(Ordering::Relaxed);
        let opps = self.opportunities_found.load(Ordering::Relaxed);
        let profitable = self.profitable_after_fees.load(Ordering::Relaxed);
        let uni = self.uniswap_swaps.load(Ordering::Relaxed);
        let aero_cl = self.aerodrome_cl_swaps.load(Ordering::Relaxed);
        let aero_v2 = self.aerodrome_v2_swaps.load(Ordering::Relaxed);
        let blocks = self.blocks_seen.load(Ordering::Relaxed);
        let tracked = self.tracked_pools_active.load(Ordering::Relaxed);
        let profit_cents = self.total_estimated_profit_cents.load(Ordering::Relaxed);
        let profit_usd = profit_cents as f64 / 100.0;

        info!("╔══════════════════════════════════════════════════╗");
        info!("║          📊 CREBTO DRY-RUN REPORT               ║");
        info!("╠══════════════════════════════════════════════════╣");
        info!("║ ⏱  Uptime: {} minutes                            ", elapsed_secs / 60);
        info!("║ 🧱 Blocks seen: {}                               ", blocks);
        info!("║ 🔄 Total swaps detected: {}                      ", swaps);
        info!("║    ├─ Uniswap V3: {}                             ", uni);
        info!("║    ├─ Aerodrome CL: {}                           ", aero_cl);
        info!("║    └─ Aerodrome V2: {}                           ", aero_v2);
        info!("║ 📋 Active tracked pools: {}                      ", tracked);
        info!("║ 🎯 Arbitrage opportunities: {}                   ", opps);
        info!("║ 💰 Profitable (after fees): {}                   ", profitable);
        info!("║ 💵 Est. total profit: ${:.2}                     ", profit_usd);
        info!("╚══════════════════════════════════════════════════╝");
    }
}

// --- CONCENTRATED LIQUIDITY MATH FOR LOCAL SIMULATION ---
// We simulate swaps locally with extreme precision and under 1 microsecond.
fn simulate_uniswap_v3_swap(
    amount_in: U256,
    zero_for_one: bool,
    sqrt_price_x96: U256,
    liquidity: u128,
    fee_bps: u32,
) -> U256 {
    if liquidity == 0 || amount_in.is_zero() {
        return U256::ZERO;
    }

    let fee_pips = fee_bps.min(1_000_000);
    let fee_factor = U256::from(1_000_000 - fee_pips);
    let amount_in_with_fee = (amount_in * fee_factor) / U256::from(1_000_000);

    // Uniswap V3 concentrated liquidity swap math (within tick):
    // L = liquidity
    // If zero_for_one (Token 0 -> Token 1):
    //   1 / sqrtPrice_new = 1 / sqrtPrice_old + amount_in / L
    //   amount_out = L * (sqrtPrice_old - sqrtPrice_new)
    // If !zero_for_one (Token 1 -> Token 0):
    //   sqrtPrice_new = sqrtPrice_old + amount_in / L
    //   amount_out = L * (1/sqrtPrice_old - 1/sqrtPrice_new)
    let q96 = U256::from(1) << 96;

    if zero_for_one {
        // 1 / sqrtPrice_new = 1 / sqrtPrice_old + amount_in / L
        // Let's compute in high precision
        let inv_price_old = (q96 * q96) / sqrt_price_x96;
        let delta_inv = (amount_in_with_fee * q96) / U256::from(liquidity);
        let inv_price_new = inv_price_old + delta_inv;
        let sqrt_price_new = (q96 * q96) / inv_price_new;

        if sqrt_price_new >= sqrt_price_x96 {
            return U256::ZERO;
        }
        let delta_price = sqrt_price_x96 - sqrt_price_new;
        (U256::from(liquidity) * delta_price) / q96
    } else {
        // sqrtPrice_new = sqrtPrice_old + amount_in / L
        let delta_price = (amount_in_with_fee * q96) / U256::from(liquidity);
        let sqrt_price_new = sqrt_price_x96 + delta_price;

        let inv_price_old = (q96 * q96) / sqrt_price_x96;
        let inv_price_new = (q96 * q96) / sqrt_price_new;

        if inv_price_old <= inv_price_new {
            return U256::ZERO;
        }
        let delta_inv = inv_price_old - inv_price_new;
        (U256::from(liquidity) * delta_inv) / q96
    }
}

fn simulate_aerodrome_v2_swap(
    amount_in: U256,
    reserve_in: U256,
    reserve_out: U256,
    fee_bps: u32,
) -> U256 {
    if reserve_in.is_zero() || reserve_out.is_zero() || amount_in.is_zero() {
        return U256::ZERO;
    }
    let amount_in_with_fee = amount_in * U256::from(10000 - fee_bps);
    let numerator = amount_in_with_fee * reserve_out;
    let denominator = (reserve_in * U256::from(10000)) + amount_in_with_fee;
    numerator / denominator
}

fn sqrt_price_x96_from_reserves(reserve0: U256, reserve1: U256) -> U256 {
    if reserve0.is_zero() || reserve1.is_zero() {
        return U256::ZERO;
    }

    let r0 = u256_to_f64(reserve0);
    let r1 = u256_to_f64(reserve1);
    // raw ratio = reserve1 / reserve0 (no decimal adjustment — same as V3 sqrtPriceX96 convention)
    let raw_ratio = r1 / r0;
    let sqrt_price = raw_ratio.sqrt();
    let q96: f64 = (2.0_f64).powi(96);
    U256::from((sqrt_price * q96) as u128)
}

fn quote_first_tokens(token0: Address, token1: Address) -> (Address, Address) {
    if token0 == USDC || token1 == USDC {
        let other = if token0 == USDC { token1 } else { token0 };
        (USDC, other)
    } else if token0 == WETH || token1 == WETH {
        let other = if token0 == WETH { token1 } else { token0 };
        (WETH, other)
    } else {
        (token0, token1)
    }
}

fn amount_to_usd(amount: U256, token: Address, _decimals: u32, eth_price_usd: f64) -> Option<f64> {
    let raw = amount.to::<u128>() as f64;
    if token == USDC {
        Some(raw / 1e6)
    } else if token == WETH {
        Some((raw / 1e18) * eth_price_usd)
    } else {
        None
    }
}

#[allow(dead_code)]
fn usd_to_amount(usd_val: f64, token: Address, decimals: u32, eth_price_usd: f64) -> Option<U256> {
    let raw_val = if token == USDC {
        usd_val * 1e6
    } else if token == WETH {
        (usd_val / eth_price_usd) * 1e18
    } else {
        let decimals_factor = 10_u128.checked_pow(decimals)?;
        usd_val * decimals_factor as f64
    };

    Some(U256::from(raw_val as u128))
}

/// Pre-cache the USD→token conversion factor to avoid recomputing in simulation loops.
fn usd_conversion_factor(token: Address, decimals: u32, eth_price_usd: f64) -> f64 {
    let factor = if token == USDC {
        1e6
    } else if token == WETH {
        1e18 / eth_price_usd
    } else {
        10_u128.pow(decimals) as f64
    };
    // Clamp to prevent overflow when eth_price_usd is pathologically small
    factor.min(1e22)
}

/// Check if a token pair includes a volatile meme coin.
/// Meme coins have higher profit sanity caps (20% vs 5%) to allow catching flash crashes.
fn is_meme_coin_pair(token0: Address, token1: Address) -> bool {
    let meme_coins = [BRETT, DEGEN, TOSHI, MIGGLES];
    meme_coins.contains(&token0) || meme_coins.contains(&token1)
}

/// Calculate price impact for a swap input on a given pool.
/// For V2: exact formula = amount_in / (reserve_in + amount_in)
/// For V3/CL: conservative approximation using liquidity as depth proxy.
///   V3 liquidity is concentrated in ticks; a large swap traverses multiple
///   ticks with decreasing liquidity. The actual impact is typically 2–5× higher
///   than the simple amount/liquidity ratio. We apply a 3× safety multiplier.
fn calculate_price_impact(pool: &TrackedPool, amount_in: U256, token_in: Address) -> f64 {
    if pool.dex_name == "aerodrome_v2" {
        let reserve_in = if pool.token0 == token_in {
            pool.reserve0
        } else {
            pool.reserve1
        };
        let amount = u256_to_f64(amount_in);
        let reserve = u256_to_f64(reserve_in);
        if reserve + amount <= 0.0 {
            return 1.0;
        }
        amount / (reserve + amount)
    } else {
        // V3/CL: use liquidity as a proxy for depth with 3× conservative multiplier
        let amount = u256_to_f64(amount_in);
        let liquidity = pool.liquidity as f64;
        if liquidity == 0.0 {
            return 1.0;
        }
        let base_impact = amount / (liquidity + amount);
        (base_impact * 3.0).min(1.0)
    }
}

// --- OPTIMIZATION ALGORITHM (BINARY SEARCH WITH PRICE IMPACT) ---
// Finds the optimal flash loan size between $10 and $1000 that maximizes net profit
// while keeping price impact on both pools below 5%.
fn optimize_loan_size(
    pool_a: &TrackedPool,
    pool_b: &TrackedPool,
    token_in: Address,
    token_out: Address,
    token_in_decimals: u32,
    eth_price_usd: f64,
    gas_cost_usd: f64,
) -> (U256, f64) {
    let start_time = Instant::now();

    if token_in != USDC && token_in != WETH {
        return (U256::ZERO, 0.0);
    }

    // Binary search range: $10 to $1,000
    let min_usd = 10.0;
    let max_usd = 1000.0;
    let max_price_impact = 0.05; // 5%

    // Pre-cache the USD→token conversion factor to avoid recomputing in every iteration
    let conversion_factor = usd_conversion_factor(token_in, token_in_decimals, eth_price_usd);

    let get_units = |usd_val: f64| -> U256 {
        U256::from((usd_val * conversion_factor) as u128)
    };

    // Meme coin pairs have higher profit sanity cap (30% vs 5%)
    // 30% catches legitimate 5–25% flash-crash spreads while rejecting extreme outliers
    let max_profit_ratio = if is_meme_coin_pair(token_in, token_out) {
        0.30
    } else {
        0.05
    };

    let mut low = min_usd;
    let mut high = max_usd;
    let mut best_size = U256::ZERO;
    let mut best_profit = 0.0;

    // 8 iterations of binary search for convergence
    for iter in 0..8 {
        // Early exit: if no profitable size found after 4 iterations and we're near the floor,
        // this spread is likely below breakeven for all sizes — stop wasting cycles.
        if iter >= 4 && best_profit <= 0.0 && (high - low) < 50.0 {
            break;
        }
        let mid = (low + high) / 2.0;
        let size = get_units(mid);

        if size.is_zero() {
            break;
        }

        // 1. Simulate swap on Pool A
        let zero_for_one_a = pool_a.token0 == token_in;
        let amount_out_a = if pool_a.dex_name == "aerodrome_v2" {
            let (reserve_in, reserve_out) = if zero_for_one_a {
                (pool_a.reserve0, pool_a.reserve1)
            } else {
                (pool_a.reserve1, pool_a.reserve0)
            };
            simulate_aerodrome_v2_swap(size, reserve_in, reserve_out, pool_a.fee_bps)
        } else {
            simulate_uniswap_v3_swap(size, zero_for_one_a, pool_a.sqrt_price_x96, pool_a.liquidity, pool_a.fee_bps)
        };

        if amount_out_a.is_zero() {
            high = mid;
            continue;
        }

        // 2. Check price impact on Pool A
        let impact_a = calculate_price_impact(pool_a, size, token_in);
        if impact_a > max_price_impact {
            // Too large for pool A depth — reduce upper bound
            high = mid;
            continue;
        }

        // 3. Simulate swap on Pool B
        let zero_for_one_b = pool_b.token0 == token_out;
        let amount_out_b = if pool_b.dex_name == "aerodrome_v2" {
            let (reserve_in, reserve_out) = if zero_for_one_b {
                (pool_b.reserve0, pool_b.reserve1)
            } else {
                (pool_b.reserve1, pool_b.reserve0)
            };
            simulate_aerodrome_v2_swap(amount_out_a, reserve_in, reserve_out, pool_b.fee_bps)
        } else {
            simulate_uniswap_v3_swap(amount_out_a, zero_for_one_b, pool_b.sqrt_price_x96, pool_b.liquidity, pool_b.fee_bps)
        };

        if amount_out_b.is_zero() {
            high = mid;
            continue;
        }

        // 4. Check price impact on Pool B (using intermediate token as input)
        let impact_b = calculate_price_impact(pool_b, amount_out_a, token_out);
        if impact_b > max_price_impact {
            // Too large for pool B depth — reduce upper bound
            high = mid;
            continue;
        }

        // 5. Calculate net profit after flash loan fee (0.05% = 5 bps)
        let flash_fee = (size * U256::from(5)) / U256::from(10000);
        if amount_out_b <= size + flash_fee {
            low = mid;
            continue;
        }

        let gross_profit = amount_out_b - size - flash_fee;
        let gross_usd = amount_to_usd(gross_profit, token_in, token_in_decimals, eth_price_usd).unwrap_or(0.0);
        let size_usd = amount_to_usd(size, token_in, token_in_decimals, eth_price_usd).unwrap_or(0.0);

        // 6. Sanity cap: meme coins allow 20%, stablecoins 5%
        if !gross_usd.is_finite() || gross_usd <= 0.0 || gross_usd > size_usd * max_profit_ratio {
            low = mid;
            continue;
        }

        // 7. Net profit after gas
        let net_profit = gross_usd - gas_cost_usd;

        if net_profit > best_profit {
            best_profit = net_profit;
            best_size = size;
        }

        // 8. Binary search direction: if profitable, try larger; if not, try smaller
        if net_profit > 0.0 {
            low = mid;
        } else {
            high = mid;
        }
    }

    let elapsed = start_time.elapsed().as_secs_f64() * 1000.0;
    if elapsed > 1.0 {
        warn!("Simulation took {:.4} ms (target < 1 ms)", elapsed);
    }

    (best_size, best_profit)
}

// --- POOL REGISTER AND STORAGE MANAGER ---
pub struct PoolRegistry {
    pools: HashMap<Address, TrackedPool>,
    pair_to_pools: HashMap<(Address, Address), Vec<Address>>,
    decimals_cache: HashMap<Address, u32>,
}

impl PoolRegistry {
    fn new() -> Self {
        let mut registry = Self {
            pools: HashMap::new(),
            pair_to_pools: HashMap::new(),
            decimals_cache: HashMap::new(),
        };
        registry.load_known_pools();
        registry
    }

    fn load_known_pools(&mut self) {
        self.decimals_cache.insert(WETH, 18);
        self.decimals_cache.insert(USDC, 6);
        self.decimals_cache.insert(AERO, 18);
        self.decimals_cache.insert(BRETT, 18);
        self.decimals_cache.insert(DEGEN, 18);
        self.decimals_cache.insert(TOSHI, 18);
        self.decimals_cache.insert(MIGGLES, 18);

        // === WETH/USDC pools ===
        self.register_pool(address!("d0b53d9277642d899df5c87a3966a349a798f224"), WETH, USDC, 500, "uniswap_v3");
        self.register_pool(address!("b4cb800910b228ed3d0834cf79d697127bbb00e5"), WETH, USDC, 3000, "uniswap_v3");
        self.register_pool(address!("b2cc224c1c9fee385f8ad6a55b4d94e92359dc59"), WETH, USDC, 100, "aerodrome_cl");
        self.register_pool(address!("dbc6998296caa1652a810dc8d3baf4a8294330f1"), WETH, USDC, 500, "aerodrome_cl");

        // === BRETT/WETH pools ===
        self.register_pool(address!("4e829f8a5213c42535ab84aa40bd4adcce9cba02"), BRETT, WETH, 10000, "aerodrome_cl");   // Slipstream 1%
        self.register_pool(address!("ba3f945812a83471d709bce9c3ca699a19fb46f7"), BRETT, WETH, 10000, "uniswap_v3");      // Uni V3 1%
        self.register_pool(address!("76bf0abd20f1e0155ce40a62615a90a709a6c3d8"), BRETT, WETH, 3000, "uniswap_v3");       // Uni V3 0.3%

        // === DEGEN/WETH pools ===
        self.register_pool(address!("c9034c3e7f58003e6ae0c8438e7c8f4598d5acaa"), DEGEN, WETH, 3000, "uniswap_v3");      // Uni V3 0.3%  $1.4M liquidity
        self.register_pool(address!("afb62448929664bfccb0aae22f232520e765ba88"), DEGEN, WETH, 3000, "aerodrome_cl");     // Aero Slipstream $18k

        // === AERO/USDC pools (from previous research) ===
        self.register_pool(address!("6cDAcb3025D68e11c3e24383B69B18B3cc2F43D8"), AERO, USDC, 200, "aerodrome_cl");      // Aero Slipstream
        self.register_pool(address!("9809e877192B0B18E1CC0a3F5110093D29B8C84A"), AERO, WETH, 3000, "aerodrome_cl");      // Aero Slipstream

        info!("📋 Pool Registry loaded: {} known pools across {} pairs",
            self.pools.len(),
            self.pair_to_pools.len()
        );
    }

    async fn warm_decimals<P: Provider<PubSubFrontend>>(&mut self, provider: &P) {
        let tokens: Vec<Address> = self.pools.values()
            .flat_map(|p| [p.token0, p.token1])
            .filter(|t| !self.decimals_cache.contains_key(t))
            .collect::<std::collections::HashSet<_>>()
            .into_iter()
            .collect();

        // Fetch all decimals in parallel for speed
        let mut futures = Vec::new();
        for token in tokens.clone() {
            let contract = IERC20::new(token, provider);
            futures.push(async move {
                match contract.decimals().call().await {
                    Ok(result) => (token, result._0 as u32),
                    Err(_) => (token, 18),
                }
            });
        }
        let results = futures_util::future::join_all(futures).await;
        for (token, decimals) in results {
            if decimals == 18 {
                warn!("⚠️ Failed to fetch decimals for {:?}, defaulting to 18", token);
            } else {
                info!("🔢 Decimals for {:?}: {}", token, decimals);
            }
            self.decimals_cache.insert(token, decimals);
        }
    }

    fn register_pool(&mut self, address: Address, token0: Address, token1: Address, fee: u32, dex: &str) {
        self.register_pool_with_state(address, token0, token1, fee, dex, U256::ZERO, U256::ZERO, false);
    }

    fn register_pool_with_state(
        &mut self,
        address: Address,
        token0: Address,
        token1: Address,
        fee: u32,
        dex: &str,
        reserve0: U256,
        reserve1: U256,
        stable: bool,
    ) {
        let key = if token0 < token1 { (token0, token1) } else { (token1, token0) };
        let entry = self.pair_to_pools.entry(key).or_default();
        if !entry.contains(&address) {
            entry.push(address);
        }

        let sqrt_price_x96 = if dex == "aerodrome_v2" {
            sqrt_price_x96_from_reserves(reserve0, reserve1)
        } else {
            U256::ZERO
        };

        self.pools.insert(
            address,
            TrackedPool {
                address,
                token0,
                token1,
                sqrt_price_x96,
                liquidity: 0,
                tick: 0,
                fee_bps: fee,
                tick_spacing: None,
                dex_name: dex.to_string(),
                last_update_block: 0,
                reserve0,
                reserve1,
                stable,
            },
        );
    }

    fn update_from_swap(&mut self, pool_address: Address, sqrt_price_x96: U256, liquidity: u128, tick: i32, block: u64) -> Vec<ArbOpportunity> {
        let (token0, token1, dex_name, sqrt_price_x96_val) = if let Some(pool) = self.pools.get_mut(&pool_address) {
            pool.sqrt_price_x96 = sqrt_price_x96;
            pool.liquidity = liquidity;
            pool.tick = tick;
            pool.last_update_block = block;
            (pool.token0, pool.token1, pool.dex_name.clone(), pool.sqrt_price_x96)
        } else {
            return vec![];
        };

        self.find_arb_opportunities(pool_address, token0, token1, &dex_name, sqrt_price_x96_val)
    }

    /// Update a V2 pool from its Swap event data (amount0In, amount1In, amount0Out, amount1Out)
    /// and compute a synthetic sqrtPriceX96 for spread comparison with V3/CL pools.
    fn update_from_v2_swap(&mut self, pool_address: Address, amount0_in: U256, amount1_in: U256, amount0_out: U256, amount1_out: U256, block: u64) -> Vec<ArbOpportunity> {
        let (token0, token1, dex_name, sqrt_price_x96_val) = if let Some(pool) = self.pools.get_mut(&pool_address) {
            // Update reserves based on swap deltas
            // reserve_new = reserve_old + amountIn - amountOut
            pool.reserve0 = pool.reserve0.saturating_add(amount0_in).saturating_sub(amount0_out);
            pool.reserve1 = pool.reserve1.saturating_add(amount1_in).saturating_sub(amount1_out);
            pool.last_update_block = block;

            // Compute synthetic sqrtPriceX96 from reserves for spread comparison.
            let synthetic_sqrt = sqrt_price_x96_from_reserves(pool.reserve0, pool.reserve1);
            pool.sqrt_price_x96 = synthetic_sqrt;

            (pool.token0, pool.token1, pool.dex_name.clone(), synthetic_sqrt)
        } else {
            return vec![];
        };

        if sqrt_price_x96_val.is_zero() {
            return vec![];
        }

        self.find_arb_opportunities(pool_address, token0, token1, &dex_name, sqrt_price_x96_val)
    }

    /// Update a V2 pool from its Sync event data (reserve0, reserve1).
    /// The Sync event is emitted on every swap and mint/burn, so it serves as a reliable
    /// fallback to keep reserves up-to-date even if the Swap event is missed.
    fn update_from_v2_sync(&mut self, pool_address: Address, reserve0: U256, reserve1: U256, block: u64) {
        if let Some(pool) = self.pools.get_mut(&pool_address) {
            pool.reserve0 = reserve0;
            pool.reserve1 = reserve1;
            pool.last_update_block = block;

            let synthetic_sqrt = sqrt_price_x96_from_reserves(pool.reserve0, pool.reserve1);
            pool.sqrt_price_x96 = synthetic_sqrt;
        }
    }

    fn find_arb_opportunities(&self, pool_address: Address, token0: Address, token1: Address, dex_name: &str, sqrt_price_x96_val: U256) -> Vec<ArbOpportunity> {
        let mut opps = vec![];
        let key = if token0 < token1 { (token0, token1) } else { (token1, token0) };
        let dec0 = self.decimals_cache.get(&token0).cloned().unwrap_or(18);
        let dec1 = self.decimals_cache.get(&token1).cloned().unwrap_or(18);
        if let Some(pool_addresses) = self.pair_to_pools.get(&key) {
            for &addr in pool_addresses {
                if addr != pool_address {
                    if let Some(other_pool) = self.pools.get(&addr) {
                        if !other_pool.sqrt_price_x96.is_zero() {
                            // Calculate spread with normalized prices
                            let p_a = sqrt_price_to_f64(sqrt_price_x96_val, dec0, dec1);
                            let other_dec0 = self.decimals_cache.get(&other_pool.token0).cloned().unwrap_or(18);
                            let other_dec1 = self.decimals_cache.get(&other_pool.token1).cloned().unwrap_or(18);
                            let mut p_b = sqrt_price_to_f64(other_pool.sqrt_price_x96, other_dec0, other_dec1);

                            // If the other pool has opposite token ordering, invert its price
                            // so both prices represent the same direction (token1/token0 of the reference pool)
                            if other_pool.token0 != token0 && p_b > 0.0 && p_b.is_finite() {
                                p_b = 1.0 / p_b;
                            }

                            if p_a > 0.0 && p_b > 0.0 && p_a.is_finite() && p_b.is_finite() {
                                let higher = p_a.max(p_b);
                                let lower = p_a.min(p_b);
                                let spread = ((higher - lower) / lower) * 10000.0;
                                // Only report if spread is realistic (not caused by math errors)
                                if spread.is_finite() && spread > 0.0 && spread < 10000.0 {
                                    let (token_in, token_out) = quote_first_tokens(token0, token1);
                                    opps.push(ArbOpportunity {
                                        pool_a: pool_address,
                                        pool_b: other_pool.address,
                                        dex_a: dex_name.to_string(),
                                        dex_b: other_pool.dex_name.clone(),
                                        token_in,
                                        token_out,
                                        spread_bps: spread,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
        opps
    }
}

fn sqrt_price_to_f64(sqrt_price_x96: U256, dec0: u32, dec1: u32) -> f64 {
    let q96: f64 = (2.0_f64).powi(96);
    let sqrt_val = u256_to_f64(sqrt_price_x96);
    let ratio = sqrt_val / q96;
    let raw_price = ratio * ratio;
    // Adjust for decimals: actual price = (token1 / 10^dec1) / (token0 / 10^dec0)
    raw_price * (10_f64.powi(dec0 as i32) / 10_f64.powi(dec1 as i32))
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

// --- WS LISTENER AND MAIN THREAD RUNNER ---
#[tokio::main]
async fn main() -> Result<(), eyre::Report> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("╔══════════════════════════════════════════════╗");
    info!("║    🚀 Starting Crebto Arbitrage Bot v0.6    ║");
    info!("║    🕵️‍♂️ Shadow Sniper Mode: Long-Tail/Meme   ║");
    info!("╚══════════════════════════════════════════════╝");

    // Load configuration from environment
    let config = Config::load_from_env()?;
    let wss_url = config.alchemy_wss.clone();

    let mut registry = PoolRegistry::new();
    let stats = Arc::new(BotStats::new());
    let start_time = Instant::now();

    // Connect to provider
    let ws = WsConnect::new(&wss_url);
    let provider = ProviderBuilder::new().on_ws(ws).await?;
    let provider = Arc::new(provider);

    // Warm decimals for all tracked tokens from on-chain
    registry.warm_decimals(provider.as_ref()).await;
    info!("🔢 Decimals warmed for {} tokens", registry.decimals_cache.len());

    info!("DRY RUN MODE: {}", config.dry_run);
    info!("Connecting to configured WSS stream");

    info!("Connected successfully. Discovering Aerodrome V2 pools dynamically...");

    // Seed tokens for long-tail discovery (WETH/USDC pairs + meme coins)
    let seed_tokens = vec![WETH, USDC, AERO, BRETT, DEGEN, TOSHI, MIGGLES];

    // Initial WETH price for liquidity estimation (will be refined later)
    let initial_weth_price = 1719.0;

    // Discover all V2 pools with sufficient liquidity
    let discovered = PoolDiscovery::discover_long_tail_pairs(
        provider.as_ref(),
        &seed_tokens,
        initial_weth_price,
        config.min_liquidity_usd,
    ).await;

    info!("🔍 Discovered {} pools with >${:.2} liquidity", discovered.len(), config.min_liquidity_usd);

    // Initialize safety checker
    let safety_checker = TokenSafetyChecker::new(config.max_tax_bps, config.min_liquidity_usd);
    let mut safe_pools = 0;
    let mut rejected_pools = 0;

    for pool in discovered {
        // Fetch decimals for both tokens if not already cached
        for token in [pool.token0, pool.token1] {
            if !registry.decimals_cache.contains_key(&token) {
                let token_contract = IERC20::new(token, provider.as_ref());
                match token_contract.decimals().call().await {
                    Ok(result) => {
                        registry.decimals_cache.insert(token, result._0 as u32);
                    }
                    Err(_) => {
                        registry.decimals_cache.insert(token, 18);
                    }
                }
            }
        }

        let dec0 = registry.decimals_cache.get(&pool.token0).copied().unwrap_or(18);
        let dec1 = registry.decimals_cache.get(&pool.token1).copied().unwrap_or(18);

        // Run safety checks on both tokens
        let (safety0, safety1) = safety_checker.check_pair(
            provider.as_ref(),
            pool.token0,
            pool.token1,
            dec0,
            dec1,
            pool.reserve0,
            pool.reserve1,
            initial_weth_price,
        ).await;

        if safety0 != TokenSafety::Safe || safety1 != TokenSafety::Safe {
            warn!(
                "🚫 Pool {} rejected: token0={:?} ({:?}), token1={:?} ({:?})",
                pool.address, pool.token0, safety0, pool.token1, safety1
            );
            rejected_pools += 1;
            continue;
        }

        // Register the safe pool
        registry.register_pool_with_state(
            pool.address,
            pool.token0,
            pool.token1,
            pool.fee_bps,
            "aerodrome_v2",
            pool.reserve0,
            pool.reserve1,
            pool.stable,
        );
        info!(
            "✅ Safe pool registered: {} at {:?} (token0={:?}, token1={:?}, liquidity=${:.2})",
            pool.label, pool.address, pool.token0, pool.token1, pool.liquidity_usd
        );
        safe_pools += 1;
    }

    info!(
        "🏁 Discovery complete: {} safe pools registered, {} rejected (honeypot/high-tax/low-liq)",
        safe_pools, rejected_pools
    );

    // Fetch initial WETH price from a WETH/USDC V2 volatile pool (more reliable than Chainlink on Base)
    let weth_price_usd = Arc::new(RwLock::new(2500.0));
    let mut price_oracle_pool: Option<Address> = None;

    // Look for the WETH/USDC V2 volatile pool we just loaded to use as price oracle.
    // Only use volatile pools (stable==false) — stable pools use a curve-based AMM
    // where reserve ratio does NOT equal market price, so they give wildly wrong prices.
    for (addr, pool) in &registry.pools {
        if pool.dex_name == "aerodrome_v2" && !pool.stable {
            let pair = if pool.token0 < pool.token1 { (pool.token0, pool.token1) } else { (pool.token1, pool.token0) };
            if pair == (WETH, USDC) {
                price_oracle_pool = Some(*addr);
                break;
            }
        }
    }

    if let Some(oracle) = price_oracle_pool {
        let v2_pool = IAerodromeV2Pool::new(oracle, provider.as_ref());
        match v2_pool.getReserves().call().await {
            Ok(reserves) => {
                let r0 = reserves.reserve0.to::<u128>() as f64;
                let r1 = reserves.reserve1.to::<u128>() as f64;
                let price = (r1 / 1e6) / (r0 / 1e18);
                if price > 0.0 && price.is_finite() {
                    info!("🔗 ETH/USD price from V2 pool: ${:.2}", price);
                    *weth_price_usd.write().await = price;
                } else {
                    warn!("⚠️ V2 pool returned invalid price, using fallback 2500");
                }
            }
            Err(e) => {
                warn!("⚠️ Failed to fetch price from V2 pool, using fallback 2500: {}", e);
            }
        }
    } else {
        warn!("⚠️ No WETH/USDC V2 pool found for price oracle, using fallback 2500");
    }

    // Spawn periodic WETH price update task from the same V2 pool
    let price_clone = weth_price_usd.clone();
    let provider_clone = provider.clone();
    let oracle_clone = price_oracle_pool;
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            if let Some(oracle) = oracle_clone {
                let v2_pool = IAerodromeV2Pool::new(oracle, provider_clone.as_ref());
                match v2_pool.getReserves().call().await {
                    Ok(reserves) => {
                        let r0 = reserves.reserve0.to::<u128>() as f64;
                        let r1 = reserves.reserve1.to::<u128>() as f64;
                        let price = (r1 / 1e6) / (r0 / 1e18);
                        if price > 0.0 && price.is_finite() {
                            *price_clone.write().await = price;
                        }
                    }
                    Err(_) => {}
                }
            }
        }
    });

    // Spawn periodic gas price update task
    let gas_price_wei = Arc::new(AtomicU64::new(50_000_000));
    let gas_price_clone = gas_price_wei.clone();
    let provider_clone = provider.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(5));
        loop {
            interval.tick().await;
            match provider_clone.get_gas_price().await {
                Ok(price) => {
                    gas_price_clone.store(price as u64, Ordering::Relaxed);
                }
                Err(_) => {}
            }
        }
    });

    // Spawn report thread
    let stats_clone = stats.clone();
    let price_for_report = weth_price_usd.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let elapsed = start_time.elapsed().as_secs();
            let price = *price_for_report.read().await;
            stats_clone.print_report(elapsed);
            info!("🔗 Current ETH/USD price: ${:.2}", price);
        }
    });

    info!("Subscribing to events for {} tracked pools...", registry.pools.len());

    let filter = Filter::new()
        .address(registry.pools.keys().cloned().collect::<Vec<_>>())
        .event_signature(vec![
            UNISWAP_V3_SWAP_TOPIC,
            AERODROME_V2_SWAP_TOPIC,
            AERODROME_V2_SYNC_TOPIC,
        ]);

    let mut sub_logs = provider.subscribe_logs(&filter).await?.into_stream();
    let mut sub_blocks = provider.subscribe_blocks().await?.into_stream();

    stats.tracked_pools_active.store(registry.pools.len() as u32, Ordering::Relaxed);

    let mut current_block = 0u64;

    loop {
        tokio::select! {
            Some(block) = sub_blocks.next() => {
                current_block = block.header.number;
                stats.blocks_seen.fetch_add(1, Ordering::Relaxed);
            }
            Some(log) = sub_logs.next() => {
                // Decode log parameters
                let topics = log.topics();
                if topics.len() < 1 { continue; }

                let pool_address = log.address();
                let topic0 = topics[0];

                // Determine which event type this is and decode accordingly
                let mut opportunities = Vec::new();
                if topic0 == UNISWAP_V3_SWAP_TOPIC {
                    // This covers both Uniswap V3 AND Aerodrome CL (Slipstream) pools
                    // since they emit the exact same Swap event signature
                    stats.swaps_detected.fetch_add(1, Ordering::Relaxed);
                    let is_aero_cl = registry.pools.get(&pool_address)
                        .map(|p| p.dex_name == "aerodrome_cl")
                        .unwrap_or(false);

                    if is_aero_cl {
                        stats.aerodrome_cl_swaps.fetch_add(1, Ordering::Relaxed);
                    } else {
                        stats.uniswap_swaps.fetch_add(1, Ordering::Relaxed);
                    }

                    let log_data = log.data();
                    if log_data.data.len() >= 128 {
                        // V3/CL layout: amount0(32) | amount1(32) | sqrtPriceX96(32) | liquidity(32) | tick(32)
                        let sqrt_price = U256::from_be_slice(&log_data.data[64..96]);
                        let liquidity = U256::from_be_slice(&log_data.data[96..128]).to::<u128>();

                        if !sqrt_price.is_zero() {
                            opportunities = registry.update_from_swap(pool_address, sqrt_price, liquidity, 0, current_block);
                        }
                    }
                } else if topic0 == AERODROME_V2_SWAP_TOPIC {
                    // Aerodrome V2 Swap event:
                    // topics: [SwapTopic, sender, to]
                    // data: amount0In(32) | amount1In(32) | amount0Out(32) | amount1Out(32)
                    stats.swaps_detected.fetch_add(1, Ordering::Relaxed);
                    stats.aerodrome_v2_swaps.fetch_add(1, Ordering::Relaxed);

                    let log_data = log.data();
                    if log_data.data.len() >= 128 {
                        let amount0_in  = U256::from_be_slice(&log_data.data[0..32]);
                        let amount1_in  = U256::from_be_slice(&log_data.data[32..64]);
                        let amount0_out = U256::from_be_slice(&log_data.data[64..96]);
                        let amount1_out = U256::from_be_slice(&log_data.data[96..128]);

                        opportunities = registry.update_from_v2_swap(pool_address, amount0_in, amount1_in, amount0_out, amount1_out, current_block);
                    }
                } else if topic0 == AERODROME_V2_SYNC_TOPIC {
                    // Aerodrome V2 Sync event:
                    // topics: [SyncTopic]
                    // data: reserve0(32) | reserve1(32)
                    // Sync is emitted on every swap, so it keeps reserves fresh even if we miss the Swap event.
                    // We do NOT count this as a swap or trigger opportunities — it just updates reserves.
                    let log_data = log.data();
                    if log_data.data.len() >= 64 {
                        let reserve0 = U256::from_be_slice(&log_data.data[0..32]);
                        let reserve1 = U256::from_be_slice(&log_data.data[32..64]);
                        registry.update_from_v2_sync(pool_address, reserve0, reserve1, current_block);
                    }
                    continue; // Skip opportunity processing for Sync events
                } else {
                    continue;
                }

                // Use cached dynamic gas price (updated in background task)
                let gas_cost_usd = (gas_price_wei.load(Ordering::Relaxed) as f64
                    * config.execution_gas_limit as f64 / 1e18)
                    * *weth_price_usd.read().await;

                for opp in opportunities {
                    stats.opportunities_found.fetch_add(1, Ordering::Relaxed);

                    // Fetch associated pool states
                    let pool_a = registry.pools.get(&opp.pool_a).unwrap();
                    let pool_b = registry.pools.get(&opp.pool_b).unwrap();

                    let dec_in = registry.decimals_cache.get(&opp.token_in).cloned().unwrap_or(18);

                    let weth_price = *weth_price_usd.read().await;

                    // Perform local EVM simulation and Dynamic Binary Search optimization
                    let sim_start = Instant::now();
                    let (opt_size, opt_profit_usd) = optimize_loan_size(
                        pool_a, pool_b, opp.token_in, opp.token_out, dec_in, weth_price, gas_cost_usd
                    );
                    let sim_time_ms = sim_start.elapsed().as_secs_f64() * 1000.0;

                    // opt_profit_usd is already net of flash loan fees, pool fees, and gas costs
                    if opt_profit_usd >= config.min_profit_usd {
                        stats.profitable_after_fees.fetch_add(1, Ordering::Relaxed);
                        stats.total_estimated_profit_cents.fetch_add((opt_profit_usd * 100.0) as u64, Ordering::Relaxed);

                        let size_usd = amount_to_usd(opt_size, opp.token_in, dec_in, weth_price).unwrap_or(0.0);
                        let aave_fee_usd = size_usd * 0.0005;

                        println!("\n[🎯 DRY RUN OPPORTUNITY DETECTED]");
                        println!("- Path: {:?} -> {:?} -> {:?}", opp.token_in, opp.token_out, opp.token_in);
                        println!("- Optimal Flash Loan Size: ${:.2}", size_usd);
                        println!("- Aave V3 Fee (0.05%): ${:.4}", aave_fee_usd);
                        println!("- Estimated Gas Cost: ${:.4} (Base L2 dynamic)", gas_cost_usd);
                        println!("- Projected NET PROFIT to Wallet: ${:.2}", opt_profit_usd);
                        println!("- Latency (Simulation Time): {:.4} ms\n", sim_time_ms);
                    }
                }
            }
        }
    }
}
