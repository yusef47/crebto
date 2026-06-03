mod config;
mod dex;
mod stream;
mod simulator;
mod strategy;
mod executor;
mod alerts;

use config::Config;
use stream::{WsListener, SwapDecoder, DecodedSwap};
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
        }
    }

    fn print_report(&self, elapsed_secs: u64) {
        let swaps = self.swaps_detected.load(Ordering::Relaxed);
        let opps = self.opportunities_found.load(Ordering::Relaxed);
        let profitable = self.profitable_after_fees.load(Ordering::Relaxed);
        let uni = self.uniswap_swaps.load(Ordering::Relaxed);
        let aero = self.aerodrome_swaps.load(Ordering::Relaxed);
        let blocks = self.blocks_seen.load(Ordering::Relaxed);

        info!("╔══════════════════════════════════════════════╗");
        info!("║        📊 CREBTO DRY-RUN REPORT             ║");
        info!("╠══════════════════════════════════════════════╣");
        info!("║ ⏱  Uptime: {} minutes                       ", elapsed_secs / 60);
        info!("║ 🧱 Blocks seen: {}                          ", blocks);
        info!("║ 🔄 Total swaps detected: {}                 ", swaps);
        info!("║    ├─ Uniswap V3: {}                        ", uni);
        info!("║    └─ Aerodrome:  {}                        ", aero);
        info!("║ 🎯 Arbitrage opportunities: {}              ", opps);
        info!("║ 💰 Profitable (after fees): {}              ", profitable);
        info!("╚══════════════════════════════════════════════╝");
    }
}

#[tokio::main]
async fn main() -> Result<(), eyre::Report> {
    // 1. Initialize Logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("╔══════════════════════════════════════════════╗");
    // Crebto main entrypoint
    info!("Starting Crebto Arbitrage Bot...");
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

    // 4. Setup Channels
    let (log_tx, mut log_rx) = mpsc::channel::<Log>(500);
    let (block_tx, mut block_rx) = mpsc::channel::<u64>(50);

    // 5. Start WebSocket Listener (Collector)
    let listener = WsListener::new(config.alchemy_wss.clone());
    listener.listen(log_tx, block_tx).await?;
    info!("✅ Connected to Base L2 via Alchemy WSS");

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

    info!("🎧 Listening for swap events on Base L2...");
    info!("   Monitoring: Uniswap V3 + Aerodrome");
    info!("   Press Ctrl+C to stop.\n");

    // 6. Event Loop
    loop {
        // Kill Switch
        let failures = stats.consecutive_failures.load(Ordering::SeqCst);
        if failures >= config.max_consecutive_failures {
            let msg = format!("🚨 KILL SWITCH: {} consecutive failures. Shutting down.", failures);
            error!("{}", msg);
            if let Some(ref n) = notifier {
                n.send_message(&msg).await;
            }
            break;
        }

        tokio::select! {
            // New Blocks
            Some(block_number) = block_rx.recv() => {
                stats.blocks_seen.fetch_add(1, Ordering::Relaxed);
                // Log every 10th block to avoid spam
                if stats.blocks_seen.load(Ordering::Relaxed) % 10 == 0 {
                    info!("🧱 Block #{} (total blocks: {})", 
                        block_number, 
                        stats.blocks_seen.load(Ordering::Relaxed)
                    );
                }
            }

            // Incoming Swap Logs
            Some(log) = log_rx.recv() => {
                stats.swaps_detected.fetch_add(1, Ordering::Relaxed);

                let decoded = match SwapDecoder::decode(&log) {
                    Ok(d) => d,
                    Err(_) => continue,
                };

                match decoded {
                    DecodedSwap::UniswapV3 { pool, amount0, amount1, sqrt_price_x96, liquidity, tick, .. } => {
                        stats.uniswap_swaps.fetch_add(1, Ordering::Relaxed);

                        // Only log significant swaps (avoid spam from tiny trades)
                        if amount0 > U256::from(1_000_000u64) || amount1 > U256::from(1_000_000u64) {
                            info!("🔵 UniV3 Swap | Pool: {:#x} | amt0: {} | amt1: {} | tick: {}",
                                pool, amount0, amount1, tick
                            );
                        }

                        // TODO: In next phase, check if this pool has a matching pool 
                        // on Aerodrome/SushiSwap for arbitrage
                    }

                    DecodedSwap::Aerodrome { pool, amount0_in, amount1_in, amount0_out, amount1_out } => {
                        stats.aerodrome_swaps.fetch_add(1, Ordering::Relaxed);

                        if amount0_in > U256::from(1_000_000u64) || amount1_in > U256::from(1_000_000u64) {
                            info!("🟢 Aero Swap  | Pool: {:#x} | in0: {} | in1: {} | out0: {} | out1: {}",
                                pool, amount0_in, amount1_in, amount0_out, amount1_out
                            );
                        }
                    }
                }
            }
        }
    }

    // Final report
    let elapsed = start_time.elapsed().as_secs();
    stats.print_report(elapsed);

    Ok(())
}
