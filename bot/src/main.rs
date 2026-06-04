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
use strategy::PoolTracker;
use alerts::TelegramNotifier;

use alloy::{
    rpc::types::eth::Log,
    primitives::{Address, U256},
};
use tokio::sync::mpsc;
use tracing::{info, warn, error, Level};
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Instant;

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
                                    stats.uniswap_swaps.fetch_add(1, Ordering::Relaxed);

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
                                        if opp.estimated_profit_usd > 0.10 {
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
                                                info!("🚀 EXECUTING arbitrage (LIVE MODE)...");
                                                // TODO: Execute via FlashArb contract
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
