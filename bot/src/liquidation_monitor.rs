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
            let poll_start = std::time::Instant::now();
            let mut scanned = 0usize;
            let mut candidates_found = 0usize;

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
                scanned += batch.len();

                for (user, result) in results {
                    let hf = result.healthFactor;
                    let debt_usd = result.totalDebtBase;

                    // Filter: micro-liquidation criteria (USD thresholds)
                    if hf < U256::from(config.hf_threshold)
                        && debt_usd >= U256::from(config.min_debt_usd)
                        && debt_usd <= U256::from(config.max_debt_usd)
                    {
                        // ─── Dynamic asset discovery ───
                        // 1. Get the list of assets this user has interacted with
                        let reserves = match pool.getUserReservesList(user).call().await {
                            Ok(r) => r._0,
                            Err(e) => {
                                warn!("Failed to getUserReservesList for user={:?}: {}", user, e);
                                continue;
                            }
                        };

                        if reserves.is_empty() {
                            info!("User {:?} has no reserves, skipping", user);
                            continue;
                        }

                        // 2. Scan each reserve to find the best debt and collateral assets
                        let mut best_debt_asset: Option<Address> = None;
                        let mut best_debt_raw = U256::ZERO;
                        let mut best_collateral_asset: Option<Address> = None;
                        let mut best_collateral_raw = U256::ZERO;

                        for &asset in &reserves {
                            match pool.getUserReserveData(asset, user).call().await {
                                Ok(data) => {
                                    // Track highest variable debt asset
                                    if data.currentVariableDebt > best_debt_raw {
                                        best_debt_raw = data.currentVariableDebt;
                                        best_debt_asset = Some(asset);
                                    }
                                    // Track highest collateral asset (must be enabled as collateral)
                                    if data.usageAsCollateralEnabled && data.currentATokenBalance > best_collateral_raw {
                                        best_collateral_raw = data.currentATokenBalance;
                                        best_collateral_asset = Some(asset);
                                    }
                                }
                                Err(e) => {
                                    warn!(
                                        "Failed to getUserReserveData for user={:?} asset={:?}: {}",
                                        user, asset, e
                                    );
                                }
                            }
                        }

                        // Validate discovery results
                        let Some(debt_asset) = best_debt_asset else {
                            info!("User {:?} has no variable debt asset, skipping", user);
                            continue;
                        };
                        let Some(collateral_asset) = best_collateral_asset else {
                            info!("User {:?} has no usable collateral asset, skipping", user);
                            continue;
                        };

                        if best_debt_raw == U256::ZERO {
                            info!("User {:?} has zero variable debt across all assets, skipping", user);
                            continue;
                        }

                        info!(
                            "💀 Liquidation candidate: user={:?}, hf={}, debt_usd={}, debt_asset={:?}, collateral_asset={:?}, debt_raw={}",
                            user, hf, debt_usd, debt_asset, collateral_asset, best_debt_raw
                        );

                        // Aave V3 close factor: 50% unless HF < 0.95 (deep underwater)
                        let close_factor = if hf < U256::from(950_000_000_000_000_000u128) {
                            U256::from(10_000) // 100%
                        } else {
                            U256::from(5_000) // 50%
                        };
                        let debt_to_cover = (best_debt_raw * close_factor) / U256::from(10_000);

                        if debt_to_cover == U256::ZERO {
                            info!(
                                "debt_to_cover rounded to 0 for user={:?} (debt_raw={}, close_factor={}), skipping",
                                user, best_debt_raw, close_factor
                            );
                            continue;
                        }

                        let opp = LiquidationOpportunity {
                            user,
                            collateral: collateral_asset,
                            debt: debt_asset,
                            debt_to_cover,
                            total_debt_base: debt_usd,
                            health_factor: hf,
                        };

                        candidates_found += 1;

                        if tx.send(opp).await.is_err() {
                            warn!("Liquidation channel closed");
                            return Ok(());
                        }
                    }
                }
            }

            let elapsed = poll_start.elapsed().as_millis();
            info!(
                "📊 Poll complete: {} borrowers scanned, {} candidates found, {}ms elapsed",
                scanned, candidates_found, elapsed
            );

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
