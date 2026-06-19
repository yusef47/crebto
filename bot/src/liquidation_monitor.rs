use std::str::FromStr;
use alloy::{
    primitives::{Address, U256},
};
use std::sync::Arc;
use tokio::sync::mpsc;
use tokio::time::{interval, Duration};
use tracing::{info, warn};

use crate::{
    config::Config,
    contracts::IAavePool,
    rpc_rotator::RotatingProvider,
    persistence::BotCheckpoint,
};

/// Micro-liquidation opportunity detected by the monitor.
#[derive(Debug, Clone)]
pub struct LiquidationOpportunity {
    pub user: Address,
    pub collateral: Address,
    pub debt: Address,
    pub debt_to_cover: U256,
    pub total_debt_base: U256,
    pub health_factor: U256,
}

pub struct LiquidationMonitor;

impl LiquidationMonitor {
    pub async fn run(
        rpc: Arc<RotatingProvider>,
        tx: mpsc::Sender<LiquidationOpportunity>,
        config: Config,
        checkpoint: Arc<tokio::sync::RwLock<BotCheckpoint>>,
    ) -> eyre::Result<()> {
        let _aave_pool = IAavePool::new(config.aave_pool, rpc.best_provider());
        let mut tick = interval(Duration::from_secs(config.poll_interval_secs));

        // Load or build watchlist
        let watchlist = checkpoint.read().await.watchlist.clone();
        let borrowers: Vec<Address> = if watchlist.is_empty() {
            // If no checkpoint, load from file or use empty (subgraph scraper should pre-fill)
            info!("Watchlist empty — bot will wait for watchlist.json to be populated");
            vec![]
        } else {
            watchlist.iter()
                .filter_map(|s| s.parse::<Address>().ok())
                .collect()
        };

        if borrowers.is_empty() {
            warn!("No borrowers in watchlist. Bot will idle and retry every 60s. Populate watchlist.json to start scanning.");
            let mut idle = interval(Duration::from_secs(60));
            loop {
                idle.tick().await;
                warn!("Still no borrowers in watchlist. Waiting...");
            }
        }

        info!("🔍 LiquidationMonitor started: {} borrowers, {}s poll interval", borrowers.len(), config.poll_interval_secs);

        loop {
            tick.tick().await;

            // Batch multicall for health factors
            let batches = borrowers.chunks(config.multicall_batch_size);
            for batch in batches {
                let best_p = rpc.best_provider_owned();
                let pool = IAavePool::new(config.aave_pool, &best_p);
                let mut results = Vec::new();
                for &user in batch {
                    match pool.getUserAccountData(user).call().await {
                        Ok(result) => results.push((user, result)),
                        Err(_) => {}
                    }
                }

                // Hardcoded debt asset for Phase 0 (USDC on Base)
                let debt_asset = Address::from_str("0x833589fCD6eDb6E08f4c7C32D4f71b54bdA02913").unwrap();

                for (user, result) in results {
                    let hf = result.healthFactor;  // healthFactor is 6th return value (index 5)
                    let debt_usd = result.totalDebtBase;  // totalDebtBase (index 1) — USD with 8 decimals
                    let _collateral = result.totalCollateralBase;  // totalCollateralBase (index 2)

                    // Filter: micro-liquidation criteria (USD thresholds)
                    if hf < U256::from(config.hf_threshold)
                        && debt_usd >= U256::from(config.min_debt_usd)
                        && debt_usd <= U256::from(config.max_debt_usd)
                    {
                        // Query actual debt token raw amount (currentVariableDebt) from Aave
                        let debt_raw = match pool.getUserReserveData(debt_asset, user).call().await {
                            Ok(reserve_data) => reserve_data.currentVariableDebt,
                            Err(e) => {
                                warn!(
                                    "Failed to getUserReserveData for user={:?}: {}",
                                    user, e
                                );
                                continue;
                            }
                        };

                        // Skip if the user has no debt in this specific asset
                        if debt_raw == U256::ZERO {
                            info!(
                                "User {:?} has no debt in {:?} (total debt is in other assets), skipping",
                                user, debt_asset
                            );
                            continue;
                        }

                        info!(
                            "💀 Liquidation candidate: user={:?}, hf={}, debt_usd={}, debt_raw={}",
                            user, hf, debt_usd, debt_raw
                        );

                        // For simplicity, assume USDC debt and WETH collateral
                        // In production, query Aave data engine for actual assets
                        // Aave V3 close factor: 50% unless HF < 0.95 (deep underwater)
                        let close_factor = if hf < U256::from(950_000_000_000_000_000u128) {
                            U256::from(10_000) // 100% (scaled 10_000 = 100%)
                        } else {
                            U256::from(5_000) // 50%
                        };
                        let debt_to_cover = (debt_raw * close_factor) / U256::from(10_000);

                        if debt_to_cover == U256::ZERO {
                            info!(
                                "debt_to_cover rounded to 0 for user={:?} (debt_raw={}, close_factor={}), skipping",
                                user, debt_raw, close_factor
                            );
                            continue;
                        }

                        let opp = LiquidationOpportunity {
                            user,
                            collateral: Address::from_str("0x4200000000000000000000000000000000000006").unwrap(), // WETH Base
                            debt: debt_asset,
                            debt_to_cover,
                            total_debt_base: debt_usd,
                            health_factor: hf,
                        };

                        if tx.send(opp).await.is_err() {
                            warn!("Liquidation channel closed");
                            return Ok(());
                        }
                    }
                }
            }

            // Save checkpoint every 10 ticks (~100s)
            let mut cp = checkpoint.write().await;
            cp.last_run_timestamp = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if let Err(e) = cp.save(&config.checkpoint_path).await {
                warn!("Failed to save checkpoint: {}", e);
            }
        }
    }
}
