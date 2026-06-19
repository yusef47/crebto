// Crebto Micro-Liquidation Bot v0.2.1
// Base L2 — Aave V3 — Balancer Flash Loans — Kaggle Optimized

use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, Level};
use tracing_subscriber::FmtSubscriber;

mod config;
mod contracts;
mod rpc_rotator;
mod liquidation_monitor;
mod bundle_builder;
mod simulator;

mod persistence;

use crate::{
    config::Config,
    rpc_rotator::RotatingProvider,
    liquidation_monitor::LiquidationMonitor,
    bundle_builder::BundleBuilder,
    persistence::BotCheckpoint,
};

#[tokio::main]
async fn main() -> eyre::Result<()> {
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .with_writer(std::io::stderr)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("╔══════════════════════════════════════════════╗");
    info!("║    🚀 Crebto Micro-Liquidation Bot v0.2.1    ║");
    info!("║    💧 Base Aave V3 — Kaggle Optimized       ║");
    info!("╚══════════════════════════════════════════════╝");

    let config = Config::from_env()?;
    info!("Config loaded: chain_id={}, dry_run={}", config.chain_id, config.dry_run);

    // ── Load checkpoint (survives Kaggle 12h restart) ──
    let checkpoint = Arc::new(RwLock::new(BotCheckpoint::load(&config.checkpoint_path).await?));
    let cp = checkpoint.read().await;
    info!(
        "📋 Checkpoint loaded: {} wins, ${:.2} total profit",
        cp.total_wins, cp.total_profit_usd
    );
    drop(cp);

    // ── Initialize rotating RPC provider ──
    let rpc = Arc::new(RotatingProvider::new(config.rpc_urls.clone()).await?);
    info!("🔗 RotatingProvider initialized with {} endpoints", config.rpc_urls.len());

    // ── Channels ──
    let (liq_tx, liq_rx) = mpsc::channel::<crate::liquidation_monitor::LiquidationOpportunity>(128);

    // ── Spawn tasks ──
    let monitor_handle = tokio::spawn(LiquidationMonitor::run(
        rpc.clone(),
        liq_tx,
        config.clone(),
        checkpoint.clone(),
    ));

    let builder_handle = tokio::spawn(BundleBuilder::run(
        liq_rx,
        rpc.clone(),
        config.clone(),
        checkpoint.clone(),
    ));

    // ── Graceful shutdown on Ctrl+C ──
    let shutdown_cp = checkpoint.clone();
    tokio::spawn(async move {
        tokio::signal::ctrl_c().await.ok();
        info!("🛑 Shutdown signal received — saving checkpoint...");
        let cp = shutdown_cp.read().await.clone();
        let _ = cp.save(&config.checkpoint_path).await;
        std::process::exit(1);
    });

    // ── Wait for tasks ──
    let _ = tokio::try_join!(monitor_handle, builder_handle)?;

    Ok(())
}
