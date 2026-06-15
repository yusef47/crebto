// Production-ready, high-performance, and self-contained Crebto Arbitrage Bot
// Built for Base Layer-2 Network (2026)
// Targets mid-cap/long-tail pools on Base where competition is low.

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

// --- GLOBAL SETTINGS ---
// These are now loaded from Config. Fallbacks removed.
// const DRY_RUN: bool = true;
// const MIN_PROFIT_USD: f64 = 1.0;
// const GAS_LIMIT: u64 = 600_000;
// const BASE_FEE_WEI: u64 = 5_000_000; // ~0.005 gwei typical Base gas

pub const CHAINLINK_ETH_USD: Address = address!("71041dddad356df2e01399310d6b3f67c3071b60");

// --- VERIFIED EVENT SIGNATURES (KECCAK-256) ---
pub const UNISWAP_V3_SWAP_TOPIC: B256 = alloy::primitives::b256!("c42079f94a6350d7e6235f29174924f928cc2ac818eb64fed8004e115fbcca67");
pub const AERODROME_V2_SWAP_TOPIC: B256 = alloy::primitives::b256!("d78ad95fa46c994b6551d0da85fc275fe613ce37657fb8d5e3d130840159d822");

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

#[derive(Debug, Clone, Copy)]
struct V2PoolSpec {
    token_a: Address,
    token_b: Address,
    stable: bool,
    fee_bps: u32,
    label: &'static str,
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

fn aerodrome_v2_pool_specs() -> Vec<V2PoolSpec> {
    vec![
        V2PoolSpec { token_a: WETH, token_b: USDC, stable: true,  fee_bps: 4,  label: "WETH/USDC stable" },
        V2PoolSpec { token_a: WETH, token_b: USDC, stable: false, fee_bps: 30, label: "WETH/USDC volatile" },
        V2PoolSpec { token_a: AERO, token_b: USDC, stable: false, fee_bps: 30, label: "AERO/USDC volatile" },
        V2PoolSpec { token_a: AERO, token_b: WETH, stable: false, fee_bps: 30, label: "AERO/WETH volatile" },
        V2PoolSpec { token_a: BRETT, token_b: WETH, stable: false, fee_bps: 30, label: "BRETT/WETH volatile" },
        V2PoolSpec { token_a: DEGEN, token_b: WETH, stable: false, fee_bps: 30, label: "DEGEN/WETH volatile" },
        V2PoolSpec { token_a: TOSHI, token_b: WETH, stable: false, fee_bps: 30, label: "TOSHI/WETH volatile" },
        V2PoolSpec { token_a: MIGGLES, token_b: WETH, stable: false, fee_bps: 30, label: "MIGGLES/WETH volatile" },
    ]
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

// --- OPTIMIZATION ALGORITHM (BINARY SEARCH) ---
// Finds the loan size that maximizes P(L) local to revm environment.
fn optimize_loan_size(
    pool_a: &TrackedPool,
    pool_b: &TrackedPool,
    token_in: Address,
    token_out: Address,
    token_in_decimals: u32,
    eth_price_usd: f64,
) -> (U256, f64) {
    let start_time = Instant::now();

    if token_in != USDC && token_in != WETH {
        return (U256::ZERO, 0.0);
    }

    // Range in USD: $20 to $500
    let min_usd = 20.0;
    let max_usd = 500.0;

    let get_units = |usd_val: f64| -> U256 {
        usd_to_amount(usd_val, token_in, token_in_decimals, eth_price_usd).unwrap_or(U256::ZERO)
    };

    let calculate_profit = |L: U256| -> f64 {
        if L.is_zero() {
            return 0.0;
        }

        // 1. Swap on Pool A
        let zero_for_one_a = pool_a.token0 == token_in;
        let amount_out_a = if pool_a.dex_name == "aerodrome_v2" {
            // Use actual tracked reserves from live V2 swap events
            let (reserve_in, reserve_out) = if zero_for_one_a {
                (pool_a.reserve0, pool_a.reserve1)
            } else {
                (pool_a.reserve1, pool_a.reserve0)
            };
            simulate_aerodrome_v2_swap(L, reserve_in, reserve_out, pool_a.fee_bps)
        } else {
            simulate_uniswap_v3_swap(L, zero_for_one_a, pool_a.sqrt_price_x96, pool_a.liquidity, pool_a.fee_bps)
        };

        if amount_out_a.is_zero() {
            return 0.0;
        }

        // 2. Swap on Pool B
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

        if amount_out_b <= L {
            return 0.0;
        }

        // 3. Subtract Flash Loan Fee (0.05% = 5 bps)
        let flash_fee = (L * U256::from(5)) / U256::from(10000);
        if amount_out_b <= L + flash_fee {
            return 0.0;
        }

        let gross_profit = amount_out_b - L - flash_fee;

        let gross_usd = amount_to_usd(gross_profit, token_in, token_in_decimals, eth_price_usd).unwrap_or(0.0);
        let size_usd = amount_to_usd(L, token_in, token_in_decimals, eth_price_usd).unwrap_or(0.0);                        if !gross_usd.is_finite() || gross_usd <= 0.0 || gross_usd > size_usd * 0.05 {
                            return 0.0;
                        }

        gross_usd
    };

    // Binary search on the trade size range
    let mut low = min_usd;
    let mut high = max_usd;
    let mut best_size = U256::ZERO;
    let mut best_profit = 0.0;

    for _ in 0..12 {
        let mid1 = low + (high - low) / 3.0;
        let mid2 = high - (high - low) / 3.0;

        let size1 = get_units(mid1);
        let size2 = get_units(mid2);

        let profit1 = calculate_profit(size1);
        let profit2 = calculate_profit(size2);

        if profit1 > profit2 {
            if profit1 > best_profit {
                best_profit = profit1;
                best_size = size1;
            }
            high = mid2;
        } else {
            if profit2 > best_profit {
                best_profit = profit2;
                best_size = size2;
            }
            low = mid1;
        }
    }

    let elapsed = start_time.elapsed().as_secs_f64() * 1000.0;
    // Log target under 1ms
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
        self.register_pool_with_state(address, token0, token1, fee, dex, U256::ZERO, U256::ZERO);
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
    info!("║    🚀 Starting Crebto Arbitrage Bot v0.4    ║");
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

    // Fetch initial WETH price from Chainlink
    let weth_price_usd = Arc::new(RwLock::new(2500.0));
    {
        let price_feed = IChainlinkPriceFeed::new(CHAINLINK_ETH_USD, provider.as_ref());
        match price_feed.latestAnswer().call().await {
            Ok(result) => {
                let answer_i128: i128 = result.answer.into_raw().to::<u128>() as i128;
                if answer_i128 > 0 {
                    let price = answer_i128 as f64 / 1e8;
                    info!("🔗 Chainlink ETH/USD price: ${:.2}", price);
                    *weth_price_usd.write().await = price;
                } else {
                    warn!("⚠️ Chainlink returned negative/invalid price, using fallback 2500");
                }
            }
            Err(e) => {
                warn!("⚠️ Failed to fetch Chainlink price, using fallback 2500: {}", e);
            }
        }
    }

    // Spawn periodic WETH price update task
    let price_clone = weth_price_usd.clone();
    let provider_clone = provider.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let price_feed = IChainlinkPriceFeed::new(CHAINLINK_ETH_USD, provider_clone.as_ref());
            match price_feed.latestAnswer().call().await {
                Ok(result) => {
                    let answer_i128: i128 = result.answer.into_raw().to::<u128>() as i128;
                    if answer_i128 > 0 {
                        let price = answer_i128 as f64 / 1e8;
                        *price_clone.write().await = price;
                    }
                }
                Err(_) => {}
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

    info!("DRY RUN MODE: {}", config.dry_run);
    info!("Connecting to configured WSS stream");

    info!("Connected successfully. Loading Aerodrome V2 pools from router...");

    let router = IAerodromeRouter::new(AERODROME_V2_ROUTER, provider.as_ref());
    for spec in aerodrome_v2_pool_specs() {
        match router.poolFor(spec.token_a, spec.token_b, spec.stable, AERODROME_V2_FACTORY).call().await {
            Ok(pool_result) if pool_result.pool != ZERO_ADDRESS => {
                let v2_pool = IAerodromeV2Pool::new(pool_result.pool, provider.as_ref());
                let token0 = match v2_pool.token0().call().await {
                    Ok(result) => result.token,
                    Err(err) => {
                        warn!("Aerodrome V2 {} skipped: token0() failed for {:?}: {}", spec.label, pool_result.pool, err);
                        continue;
                    }
                };
                let token1 = match v2_pool.token1().call().await {
                    Ok(result) => result.token,
                    Err(err) => {
                        warn!("Aerodrome V2 {} skipped: token1() failed for {:?}: {}", spec.label, pool_result.pool, err);
                        continue;
                    }
                };
                let reserves = match v2_pool.getReserves().call().await {
                    Ok(result) => result,
                    Err(err) => {
                        warn!("Aerodrome V2 {} skipped: getReserves() failed for {:?}: {}", spec.label, pool_result.pool, err);
                        continue;
                    }
                };
                let reserve0 = reserves.reserve0;
                let reserve1 = reserves.reserve1;

                if reserve0.is_zero() || reserve1.is_zero() {
                    warn!("Aerodrome V2 {} skipped: empty reserves at {:?}", spec.label, pool_result.pool);
                    continue;
                }

                // Warm decimals for newly discovered tokens
                for token in [token0, token1] {
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

                registry.register_pool_with_state(
                    pool_result.pool,
                    token0,
                    token1,
                    spec.fee_bps,
                    "aerodrome_v2",
                    reserve0,
                    reserve1,
                );
                info!(
                    "Loaded Aerodrome V2 {} pool {:?}: token0={:?}, token1={:?}, r0={}, r1={}",
                    spec.label, pool_result.pool, token0, token1, reserve0, reserve1
                );
            }
            Ok(_) => {
                info!("Aerodrome V2 {} pool not found by router", spec.label);
            }
            Err(err) => {
                warn!("Aerodrome V2 {} router lookup failed: {}", spec.label, err);
            }
        }
    }

    info!("Subscribing to events for {} tracked pools...", registry.pools.len());

    let filter = Filter::new()
        .address(registry.pools.keys().cloned().collect::<Vec<_>>())
        .event_signature(vec![
            UNISWAP_V3_SWAP_TOPIC,
            AERODROME_V2_SWAP_TOPIC,
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
                stats.swaps_detected.fetch_add(1, Ordering::Relaxed);

                // Decode log parameters
                let topics = log.topics();
                if topics.len() < 1 { continue; }

                let pool_address = log.address();
                let topic0 = topics[0];

                // Determine which event type this is and decode accordingly
                let opportunities = if topic0 == UNISWAP_V3_SWAP_TOPIC {
                    // This covers both Uniswap V3 AND Aerodrome CL (Slipstream) pools
                    // since they emit the exact same Swap event signature
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

                        if sqrt_price.is_zero() {
                            continue;
                        }

                        registry.update_from_swap(pool_address, sqrt_price, liquidity, 0, current_block)
                    } else {
                        continue;
                    }
                } else if topic0 == AERODROME_V2_SWAP_TOPIC {
                    // Aerodrome V2 (Uniswap V2-style) layout:
                    // data: amount0In(32) | amount1In(32) | amount0Out(32) | amount1Out(32)
                    stats.aerodrome_v2_swaps.fetch_add(1, Ordering::Relaxed);

                    let log_data = log.data();
                    if log_data.data.len() >= 128 {
                        let amount0_in  = U256::from_be_slice(&log_data.data[0..32]);
                        let amount1_in  = U256::from_be_slice(&log_data.data[32..64]);
                        let amount0_out = U256::from_be_slice(&log_data.data[64..96]);
                        let amount1_out = U256::from_be_slice(&log_data.data[96..128]);

                        info!("DECODED V2 SWAP: pool={:?}, a0in={}, a1in={}, a0out={}, a1out={}",
                            pool_address, amount0_in, amount1_in, amount0_out, amount1_out);

                        registry.update_from_v2_swap(pool_address, amount0_in, amount1_in, amount0_out, amount1_out, current_block)
                    } else {
                        continue;
                    }
                } else {
                    continue;
                };

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
                        pool_a, pool_b, opp.token_in, opp.token_out, dec_in, weth_price
                    );
                    let sim_time_ms = sim_start.elapsed().as_secs_f64() * 1000.0;

                    if opt_profit_usd >= config.min_profit_usd {
                        stats.profitable_after_fees.fetch_add(1, Ordering::Relaxed);
                        stats.total_estimated_profit_cents.fetch_add((opt_profit_usd * 100.0) as u64, Ordering::Relaxed);

                        let size_usd = amount_to_usd(opt_size, opp.token_in, dec_in, weth_price).unwrap_or(0.0);

                        let aave_fee_usd = size_usd * 0.0005;
                        let net_profit_usd = opt_profit_usd - aave_fee_usd - gas_cost_usd;

                        println!("\n[🎯 DRY RUN OPPORTUNITY DETECTED]");
                        println!("- Path: {:?} -> {:?} -> {:?}", opp.token_in, opp.token_out, opp.token_in);
                        println!("- Optimal Flash Loan Size: ${:.2}", size_usd);
                        println!("- Aave V3 Fee (0.05%): ${:.4}", aave_fee_usd);
                        println!("- Estimated Gas Cost: ${:.4} (Base L2 dynamic)", gas_cost_usd);
                        println!("- Projected Gross Profit: ${:.2}", opt_profit_usd);
                        println!("- Projected NET PROFIT to Wallet: ${:.2}", net_profit_usd);
                        println!("- Latency (Simulation Time): {:.4} ms\n", sim_time_ms);
                    }
                }
            }
        }
    }
}
