mod config;
mod dex;
mod stream;
mod simulator;
mod strategy;
mod executor;
mod alerts;

use config::Config;
use stream::{WsListener, SwapDecoder, DecodedSwap};
use stream::ws_listener::{UNISWAP_V3_SWAP_TOPIC, AERODROME_V2_SWAP_TOPIC};
use strategy::{CandidateBuildConfig, CandidateBuilder, PoolTracker};
use simulator::TxSimulator;
use executor::{ExecutionRisk, NonceManager, RiskLimits, RiskManager, TxBuilder};
use dex::aerodrome::AerodromeQuoter;
use dex::traits::DexQuoter;
use dex::uniswap_v3::UniswapV3Quoter;
use alerts::TelegramNotifier;

use alloy::{
    network::Ethereum,
    rpc::types::eth::Log,
    primitives::{Address, U256},
    providers::{Provider, ProviderBuilder},
    sol,
    transports::Transport,
};
use tokio::sync::mpsc;
use tracing::{info, warn, error, Level};
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

sol!(
    #[sol(rpc)]
    interface IERC20 {
        function decimals() external view returns (uint8);
        function symbol() external view returns (string);
    }
);

async fn fetch_token_decimals<T, P>(
    provider: P,
    token_address: Address,
) -> Result<u32, eyre::Report>
where
    P: Provider<T, Ethereum> + Clone,
    T: Transport + Clone,
{
    let contract = IERC20::new(token_address, provider);
    match contract.decimals().call().await {
        Ok(decimals) => Ok(decimals._0 as u32),
        Err(e) => Err(eyre::eyre!("Failed to fetch decimals for token {}: {:?}", token_address, e)),
    }
}

fn gwei_to_wei(gwei: u64) -> U256 {
    U256::from(gwei) * U256::from(1_000_000_000u64)
}

fn eth_to_wei(eth: f64) -> U256 {
    U256::from((eth.max(0.0) * 1e18) as u128)
}

fn usd_to_usdc_units(usd: f64) -> U256 {
    U256::from((usd.max(0.0) * 1e6) as u128)
}

fn usd_to_weth_wei(usd: f64, eth_price_usd: f64) -> U256 {
    if eth_price_usd <= 0.0 {
        return U256::ZERO;
    }
    U256::from(((usd.max(0.0) / eth_price_usd) * 1e18) as u128)
}

fn build_probe_amounts(config: &Config, eth_price_usd: f64) -> Vec<(Address, U256)> {
    let weth: Address = "0x4200000000000000000000000000000000000006".parse().unwrap();
    let usdc: Address = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".parse().unwrap();

    let mut probes = Vec::new();
    for usd in &config.probe_sizes_usd {
        probes.push((usdc, usd_to_usdc_units(*usd)));
        probes.push((weth, usd_to_weth_wei(*usd, eth_price_usd)));
    }
    probes
}

fn min_profit_for_asset(asset: Address, min_profit_usd: f64, eth_price_usd: f64) -> U256 {
    let weth: Address = "0x4200000000000000000000000000000000000006".parse().unwrap();
    let usdc: Address = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".parse().unwrap();

    if asset == usdc {
        usd_to_usdc_units(min_profit_usd)
    } else if asset == weth {
        usd_to_weth_wei(min_profit_usd, eth_price_usd)
    } else {
        U256::MAX
    }
}

fn quoter_for<'a>(
    dex_name: &str,
    uni_quoter: &'a UniswapV3Quoter,
    aero_v2_quoter: &'a AerodromeQuoter,
    aero_cl_quoter: &'a AerodromeQuoter,
) -> Option<&'a dyn DexQuoter> {
    match dex_name {
        "uniswap_v3" | "sushiswap_v3" => Some(uni_quoter),
        "aerodrome_cl" => Some(aero_cl_quoter),
        "aerodrome_v2" | "aerodrome" => Some(aero_v2_quoter),
        _ => None,
    }
}

#[derive(Debug)]
pub struct BotStats {
    pub swaps_detected: AtomicU64,
    pub opportunities_found: AtomicU64,
    pub profitable_after_fees: AtomicU64,
    pub uniswap_swaps: AtomicU64,
    pub aerodrome_swaps: AtomicU64,
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
            aerodrome_swaps: AtomicU64::new(0),
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
        let aero = self.aerodrome_swaps.load(Ordering::Relaxed);
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
        info!("║    └─ Aerodrome CL: {}                           ", aero);
        info!("║ 📋 Active tracked pools: {}                      ", tracked);
        info!("║ 🎯 Arbitrage opportunities: {}                   ", opps);
        info!("║ 💰 Profitable (after fees): {}                   ", profitable);
        info!("║ 💵 Est. total profit: ${:.2}                     ", profit_usd);
        info!("╚══════════════════════════════════════════════════╝");
    }
}

#[tokio::main]
async fn main() -> Result<(), eyre::Report> {
    // 1. Initialize Logging to Stderr (unbuffered for real-time logs in Kaggle)
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("╔══════════════════════════════════════════════╗");
    info!("║    🚀 Starting Crebto Arbitrage Bot v0.2    ║");
    info!("╚══════════════════════════════════════════════╝");

    // 2. Load Configuration
    let config = Config::load_from_env()?;

    if config.dry_run {
        info!("⚠️  DRY RUN MODE: No real transactions will be sent!");
        info!("    Bot will monitor, analyze, and report opportunities only.");
    } else {
        info!("🔴 LIVE MODE: Bot will execute real transactions!");
    }

    // 3. Setup Telegram Notifications (optional)
    let notifier = if let (Some(token), Some(chat_id)) = (&config.telegram_bot_token, &config.telegram_chat_id) {
        let n = TelegramNotifier::new(token.clone(), chat_id.clone());
        let mode = if config.dry_run { "DRY RUN 🧪" } else { "LIVE 🔴" };
        n.send_message(&format!("🚀 *Crebto Bot Started* on Base L2!\nMode: {}", mode)).await;
        Some(n)
    } else {
        warn!("Telegram not configured. Running without alerts.");
        None
    };

    // 4. Initialize Pool Tracker (Arbitrage Engine)
    let mut pool_tracker = PoolTracker::new();
    pool_tracker.set_min_profit_usd(config.min_profit_usd);

    // Create HTTP provider for dynamic ERC20 decimal queries
    let rpc_url = config.alchemy_http.parse::<reqwest::Url>()?;
    let provider = Arc::new(ProviderBuilder::new().on_http(rpc_url));
    let simulator = TxSimulator::provider_backed(config.alchemy_http.clone());
    let nonce_manager = if !config.dry_run {
        if let Some(executor_address) = config.executor_address {
            Some(NonceManager::initialize(provider.as_ref(), executor_address).await?)
        } else {
            warn!("EXECUTOR_ADDRESS is not configured; live send will stay disabled.");
            None
        }
    } else {
        None
    };
    let mut risk_manager = RiskManager::new(RiskLimits {
        max_gas_price_wei: gwei_to_wei(config.max_gas_price_gwei),
        min_balance_wei: eth_to_wei(config.min_eth_balance),
        max_loss_per_hour_wei: usd_to_weth_wei(config.max_loss_per_hour_usd, 2500.0),
        min_net_profit_wei: U256::ZERO,
        require_simulation: config.require_simulation,
    });

    // Bot Statistics Tracker
    let stats = Arc::new(BotStats::new());
    let start_time = Instant::now();

    // Report timer — print stats every 60 seconds
    let stats_clone = stats.clone();
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(tokio::time::Duration::from_secs(60));
        loop {
            interval.tick().await;
            let elapsed = start_time.elapsed().as_secs();
            stats_clone.print_report(elapsed);
        }
    });

    // 5. Main loop with auto-reconnection
    let mut current_block: u64 = 0;
    let mut current_gas_price_wei: u64 = 50_000_000;

    loop {
        // Setup Channels
        let (log_tx, mut log_rx) = mpsc::channel::<Log>(500);
        let (block_tx, mut block_rx) = mpsc::channel::<(u64, u64)>(50);

        // Start WebSocket Listener (Collector)
        let listener = WsListener::new(config.alchemy_wss.clone());
        let addresses = pool_tracker.get_known_addresses();
        match listener.listen(log_tx, block_tx, addresses).await {
            Ok(_) => {
                info!("✅ Connected to Base L2 via Alchemy WSS");
                info!("🎧 Listening for swap events on Base L2...");
                info!("   Monitoring: Uniswap V3 + Aerodrome Slipstream");
            }
            Err(e) => {
                error!("❌ Failed to connect to WSS: {:?}. Retrying in 10s...", e);
                tokio::time::sleep(tokio::time::Duration::from_secs(10)).await;
                continue;
            }
        }

        // 6. Event Loop
        let mut disconnected = false;
        while !disconnected {
            // Kill Switch
            let failures = stats.consecutive_failures.load(Ordering::SeqCst);
            if failures >= config.max_consecutive_failures {
                let msg = format!("🚨 KILL SWITCH: {} consecutive failures. Shutting down.", failures);
                error!("{}", msg);
                if let Some(ref n) = notifier {
                    n.send_message(&msg).await;
                }
                // Final report
                let elapsed = start_time.elapsed().as_secs();
                stats.print_report(elapsed);
                return Ok(());
            }

            tokio::select! {
                // New Blocks
                msg = block_rx.recv() => {
                    match msg {
                        Some((block_number, base_fee)) => {
                            current_block = block_number;
                            current_gas_price_wei = base_fee;
                            pool_tracker.update_gas_price(base_fee);
                            stats.blocks_seen.fetch_add(1, Ordering::Relaxed);
                            // Log every 10th block to avoid spam
                            if stats.blocks_seen.load(Ordering::Relaxed) % 10 == 0 {
                                info!("🧱 Block #{} | Base Fee: {} wei (total blocks: {})",
                                    block_number,
                                    base_fee,
                                    stats.blocks_seen.load(Ordering::Relaxed)
                                );
                            }
                        }
                        None => {
                            warn!("⚠️ Block channel closed. Connection lost.");
                            disconnected = true;
                        }
                    }
                }

                // Incoming Swap Logs
                msg = log_rx.recv() => {
                    match msg {
                        Some(log) => {
                            if let Ok(new_pool) = stream::NewPairWatcher::parse_new_pool(&log) {
                                let weth: Address = "0x4200000000000000000000000000000000000006".parse().unwrap();
                                let usdc: Address = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".parse().unwrap();

                                // Monitor newly launched pools only when one side is a reliable base asset.
                                if new_pool.token0 == weth || new_pool.token0 == usdc || new_pool.token1 == weth || new_pool.token1 == usdc {
                                    let dec0 = if new_pool.token0 == weth { 18 } else if new_pool.token0 == usdc { 6 } else {
                                        fetch_token_decimals(provider.as_ref().clone(), new_pool.token0).await.unwrap_or(18)
                                    };
                                    let dec1 = if new_pool.token1 == weth { 18 } else if new_pool.token1 == usdc { 6 } else {
                                        fetch_token_decimals(provider.as_ref().clone(), new_pool.token1).await.unwrap_or(18)
                                    };

                                    pool_tracker.register_token_decimals(new_pool.token0, dec0);
                                    pool_tracker.register_token_decimals(new_pool.token1, dec1);
                                    pool_tracker.register_pool_with_tick_spacing(
                                        new_pool.pool,
                                        new_pool.token0,
                                        new_pool.token1,
                                        new_pool.fee_bps,
                                        new_pool.tick_spacing,
                                        &new_pool.dex_name,
                                    );

                                    info!(
                                        "Dynamic Registry: registered pool {:?} (tokens: {:?}/{:?}, dex: {}, fee units: {})",
                                        new_pool.pool,
                                        new_pool.token0,
                                        new_pool.token1,
                                        new_pool.dex_name,
                                        new_pool.fee_bps
                                    );

                                    // Reconnect to refresh the address-filtered WebSocket subscription.
                                    info!("Reconnecting WSS to include the new pool in live swap monitoring...");
                                    disconnected = true;
                                }
                                continue;
                            }

                            if stream::NewPairWatcher::is_supported_factory(log.address()) {
                                continue;
                            }

                            stats.swaps_detected.fetch_add(1, Ordering::Relaxed);

                            let decoded = match SwapDecoder::decode(&log) {
                                Ok(d) => d,
                                Err(e) => {
                                    let topics = log.topics();
                                    if !topics.is_empty() && (topics[0] == UNISWAP_V3_SWAP_TOPIC || topics[0] == AERODROME_V2_SWAP_TOPIC) {
                                        warn!("Failed to decode swap log: {:?}", e);
                                    }
                                    continue;
                                }
                            };

                            match decoded {
                                DecodedSwap::UniswapV3 { pool, amount0, amount1, sqrt_price_x96, liquidity, tick, .. } => {
                                    let dex = pool_tracker.get_pool_dex(&pool).unwrap_or_else(|| "uniswap_v3".to_string());
                                    if dex == "aerodrome_cl" {
                                        stats.aerodrome_swaps.fetch_add(1, Ordering::Relaxed);
                                    } else {
                                        stats.uniswap_swaps.fetch_add(1, Ordering::Relaxed);
                                    }

                                    // Update pool tracker and check for arbitrage
                                    let opportunities = pool_tracker.update_from_swap(
                                        pool, sqrt_price_x96, liquidity, tick, current_block
                                    );

                                    // Update active pool count
                                    stats.tracked_pools_active.store(
                                        pool_tracker.active_pool_count() as u32, Ordering::Relaxed
                                    );

                                    // Process detected opportunities
                                    for opp in &opportunities {
                                        stats.opportunities_found.fetch_add(1, Ordering::Relaxed);

                                        // Estimate if profitable after all fees
                                        if opp.estimated_profit_usd >= config.min_profit_usd {
                                            stats.profitable_after_fees.fetch_add(1, Ordering::Relaxed);
                                            let profit_cents = (opp.estimated_profit_usd * 100.0) as u64;
                                            stats.total_estimated_profit_cents.fetch_add(profit_cents, Ordering::Relaxed);

                                            info!("🎯 ARB DETECTED! {} vs {} | spread: {:.1} bps | est profit: ${:.2} | gas: ${:.4}",
                                                opp.dex_a, opp.dex_b,
                                                opp.spread_bps, opp.estimated_profit_usd, opp.gas_cost_usd
                                            );
                                            info!("   Pool A: {:#x} ({})", opp.pool_a, opp.dex_a);
                                            info!("   Pool B: {:#x} ({})", opp.pool_b, opp.dex_b);
                                            info!("   Price A: {:.8} | Price B: {:.8}", opp.price_a, opp.price_b);

                                            // In DRY_RUN: just log. In LIVE: would execute.
                                            if !config.dry_run {
                                                let Some(contract_address) = config.contract_address else {
                                                    warn!("Skipping LIVE candidate: CONTRACT_ADDRESS is not configured.");
                                                    continue;
                                                };
                                                let Some(executor_address) = config.executor_address else {
                                                    warn!("Skipping LIVE candidate: EXECUTOR_ADDRESS is not configured.");
                                                    continue;
                                                };
                                                let Some(private_key) = config.private_key.as_ref() else {
                                                    warn!("Skipping LIVE candidate: PRIVATE_KEY is not configured.");
                                                    continue;
                                                };
                                                let Some(uniswap_router) = config.uniswap_v3_router else {
                                                    warn!("Skipping LIVE candidate: UNISWAP_V3_ROUTER is not configured.");
                                                    continue;
                                                };
                                                let Some(aero_router) = config.aerodrome_router else {
                                                    warn!("Skipping LIVE candidate: AERODROME_ROUTER is not configured.");
                                                    continue;
                                                };
                                                let Some(aero_slipstream_router) = config.aerodrome_slipstream_router else {
                                                    warn!("Skipping LIVE candidate: AERODROME_SLIPSTREAM_ROUTER is not configured.");
                                                    continue;
                                                };
                                                let Some(aero_factory) = config.aerodrome_factory else {
                                                    warn!("Skipping LIVE candidate: AERODROME_FACTORY is not configured.");
                                                    continue;
                                                };

                                                let Some(pool_a_state) = pool_tracker.get_pool_state(&opp.pool_a) else {
                                                    warn!("Skipping LIVE candidate: pool A state is not active yet.");
                                                    continue;
                                                };
                                                let Some(pool_b_state) = pool_tracker.get_pool_state(&opp.pool_b) else {
                                                    warn!("Skipping LIVE candidate: pool B state is not active yet.");
                                                    continue;
                                                };

                                                let uni_quoter = UniswapV3Quoter::new(uniswap_router);
                                                let aero_v2_quoter = AerodromeQuoter::new(aero_router, aero_factory, false);
                                                let aero_cl_quoter = AerodromeQuoter::slipstream(aero_slipstream_router, aero_factory);
                                                let Some(quoter_a) = quoter_for(
                                                    &opp.dex_a,
                                                    &uni_quoter,
                                                    &aero_v2_quoter,
                                                    &aero_cl_quoter,
                                                ) else {
                                                    warn!("Skipping LIVE candidate: unsupported DEX {}", opp.dex_a);
                                                    continue;
                                                };
                                                let Some(quoter_b) = quoter_for(
                                                    &opp.dex_b,
                                                    &uni_quoter,
                                                    &aero_v2_quoter,
                                                    &aero_cl_quoter,
                                                ) else {
                                                    warn!("Skipping LIVE candidate: unsupported DEX {}", opp.dex_b);
                                                    continue;
                                                };

                                                let tx_builder = TxBuilder::new(contract_address, private_key)?;
                                                let weth: Address = "0x4200000000000000000000000000000000000006".parse().unwrap();
                                                let usdc: Address = "0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913".parse().unwrap();
                                                let eth_price = pool_tracker.get_eth_price_usd().unwrap_or(2500.0);
                                                let probe_amounts = build_probe_amounts(&config, eth_price);
                                                let min_profit_weth = min_profit_for_asset(weth, config.min_profit_usd, eth_price);
                                                let min_profit_usdc = min_profit_for_asset(usdc, config.min_profit_usd, eth_price);

                                                let candidate_config = CandidateBuildConfig {
                                                    caller: executor_address,
                                                    contract_address,
                                                    gas_limit: config.execution_gas_limit,
                                                    gas_price: U256::from(current_gas_price_wei),
                                                    slippage_bps: config.slippage_bps,
                                                    min_profit_wei: U256::MAX,
                                                    min_profit_by_asset: vec![
                                                        (weth, min_profit_weth),
                                                        (usdc, min_profit_usdc),
                                                    ],
                                                    native_token_units_by_asset: vec![
                                                        (weth, U256::from(1_000_000_000_000_000_000u128)),
                                                        (usdc, usd_to_usdc_units(eth_price)),
                                                    ],
                                                    flash_fee_bps: 5,
                                                    flash_assets: vec![weth, usdc],
                                                };

                                                let built_candidates = CandidateBuilder::build_size_grid(
                                                    opp,
                                                    &pool_a_state,
                                                    quoter_a,
                                                    &pool_b_state,
                                                    quoter_b,
                                                    &tx_builder,
                                                    &candidate_config,
                                                    &probe_amounts,
                                                )?;

                                                let simulation_requests = built_candidates
                                                    .iter()
                                                    .map(|(_, request)| request.clone())
                                                    .collect::<Vec<_>>();

                                                if simulation_requests.is_empty() {
                                                    warn!("No executable candidate sizes survived quoting/profit filters.");
                                                    continue;
                                                }

                                                let optimization = simulator.optimize_size_grid(simulation_requests).await?;
                                                let Some(best) = optimization.best else {
                                                    warn!(
                                                        "All {} simulated candidate sizes failed or were unprofitable.",
                                                        optimization.rejected.len()
                                                    );
                                                    continue;
                                                };

                                                let wallet_balance = provider
                                                    .get_balance(executor_address)
                                                    .await
                                                    .unwrap_or(U256::ZERO);
                                                let risk = ExecutionRisk {
                                                    wallet_balance_wei: wallet_balance,
                                                    gas_price_wei: U256::from(current_gas_price_wei),
                                                    estimated_gas_units: best.simulation.gas_used,
                                                    net_profit_wei: best.net_profit_wei,
                                                    simulation_success: best.simulation.success,
                                                };

                                                if let Err(reason) = risk_manager.approve(&risk) {
                                                    warn!("Risk manager rejected execution: {}", reason);
                                                    continue;
                                                }

                                                info!(
                                                    "LIVE candidate approved by simulation: amount={} net_profit={} gas_used={}",
                                                    best.amount_in,
                                                    best.net_profit_wei,
                                                    best.simulation.gas_used
                                                );

                                                if !config.enable_live_send {
                                                    warn!("ENABLE_LIVE_SEND=false; approved transaction was not sent.");
                                                    continue;
                                                }

                                                let Some(nonce_manager) = nonce_manager.as_ref() else {
                                                    warn!("Nonce manager is not initialized; approved transaction was not sent.");
                                                    continue;
                                                };

                                                let tx_hash = tx_builder
                                                    .send_transaction(
                                                        provider.as_ref(),
                                                        nonce_manager,
                                                        best.request.call_data.clone(),
                                                        config.execution_gas_limit,
                                                        U256::from(current_gas_price_wei),
                                                        U256::ZERO,
                                                    )
                                                    .await?;

                                                info!("LIVE arbitrage transaction sent: {:?}", tx_hash);
                                            }
                                        }
                                    }
                                }

                                DecodedSwap::Aerodrome { pool, amount0_in, amount1_in, amount0_out, amount1_out } => {
                                    stats.aerodrome_swaps.fetch_add(1, Ordering::Relaxed);

                                    if amount0_in > U256::from(1_000_000u64) || amount1_in > U256::from(1_000_000u64) {
                                        info!("🟢 Aero V2 Swap | Pool: {:#x} | in0: {} | in1: {} | out0: {} | out1: {}",
                                            pool, amount0_in, amount1_in, amount0_out, amount1_out
                                        );
                                    }
                                }
                            }
                        }
                        None => {
                            warn!("⚠️ Log channel closed. Connection lost.");
                            disconnected = true;
                        }
                    }
                }
            }
        }

        // Connection lost — wait and reconnect
        warn!("🔄 Reconnecting in 5 seconds...");
        tokio::time::sleep(tokio::time::Duration::from_secs(5)).await;
    }
}
