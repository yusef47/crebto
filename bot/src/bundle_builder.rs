use std::str::FromStr;
use alloy::{
    consensus::{TxEip1559, TxEnvelope, SignableTransaction},
    eips::eip2718::Encodable2718,
    network::TxSigner,
    primitives::U256,
    providers::Provider,
    signers::local::PrivateKeySigner,
    sol_types::SolCall,
};
use std::sync::Arc;
use tokio::sync::{mpsc, RwLock};
use tracing::{info, warn};

use crate::{
    config::Config,
    contracts::IMEVExecutor,
    liquidation_monitor::LiquidationOpportunity,
    simulator::simulate_liquidation,
    rpc_rotator::RotatingProvider,
    persistence::BotCheckpoint,
};

pub struct BundleBuilder;

impl BundleBuilder {
    pub async fn run(
        mut rx: mpsc::Receiver<LiquidationOpportunity>,
        rpc: Arc<RotatingProvider>,
        config: Config,
        checkpoint: Arc<RwLock<BotCheckpoint>>,
    ) -> eyre::Result<()> {
        let mev_executor = config.executor_address;
        let aave_pool = config.aave_pool;

        let signer = if let Some(pk) = &config.bot_private_key {
            Some(PrivateKeySigner::from_str(pk)?)
        } else {
            None
        };

        while let Some(opp) = rx.recv().await {
            info!(
                "📦 Opportunity received | user={:?} | collateral={:?} | debt={:?} | debt_to_cover={} | hf={}",
                opp.user, opp.collateral, opp.debt, opp.debt_to_cover, opp.health_factor
            );

            // Step 1: Simulate via eth_call + StateOverride
            let best_provider = rpc.best_provider_owned();
            let sim_start = std::time::Instant::now();
            let simulation_passed = simulate_liquidation(
                &best_provider,
                mev_executor,
                aave_pool,
                opp.user,
                opp.collateral,
                opp.debt,
                opp.debt_to_cover,
                opp.debt_to_cover, // flashAmount = debtToCover for simplicity
            ).await.unwrap_or(false);
            let sim_elapsed = sim_start.elapsed().as_millis();

            if !simulation_passed {
                warn!(
                    "❌ Simulation FAILED | user={:?} | collateral={:?} | debt={:?} | sim_time={}ms | skipping",
                    opp.user, opp.collateral, opp.debt, sim_elapsed
                );
                continue;
            }

            info!(
                "✅ Simulation PASSED | user={:?} | sim_time={}ms",
                opp.user, sim_elapsed
            );

            if config.dry_run {
                // Estimate gas cost for reporting (rough ETH price $3,000 — Phase 0 placeholder)
                let gas_cost_eth = (config.max_gas as f64 * config.gas_price_wei as f64) / 1e18;
                let gas_cost_usd = gas_cost_eth * 3000.0;
                info!(
                    "🚫 DRY_RUN | Would broadcast liquidation for user={:?}\n  ├─ collateral: {:?}\n  ├─ debt: {:?}\n  ├─ debt_to_cover: {}\n  ├─ flash_amount: {}\n  ├─ min_amount_out: {}\n  ├─ max_gas: {}\n  ├─ gas_price: {} gwei\n  └─ estimated_gas_cost: ${:.2}",
                    opp.user,
                    opp.collateral,
                    opp.debt,
                    opp.debt_to_cover,
                    opp.debt_to_cover,
                    U256::ZERO,
                    config.max_gas,
                    config.gas_price_wei / 1_000_000_000,
                    gas_cost_usd
                );
                let mut cp = checkpoint.write().await;
                cp.total_wins += 1;
                // Phase 0: profit placeholder until real profit estimation is implemented
                cp.total_profit_usd += 0.0;
                if let Err(e) = cp.save(&config.checkpoint_path).await {
                    warn!("Failed to save checkpoint: {}", e);
                }
                continue;
            }

            // Step 2: Build transaction
            let Some(ref s) = signer else {
                warn!("No private key configured, cannot submit live bundles");
                continue;
            };

            let call_data = IMEVExecutor::executeLiquidationBalancerCall {
                aavePool: aave_pool,
                collateral: opp.collateral,
                debt: opp.debt,
                user: opp.user,
                debtToCover: opp.debt_to_cover,
                flashAmount: opp.debt_to_cover,
                minAmountOut: U256::ZERO, // Phase 0: accept any output; bot will calc off-chain in Phase 1
            }
            .abi_encode();

            // Fetch nonce and block number from chain
            let nonce = best_provider
                .get_transaction_count(s.address())
                .await
                .unwrap_or(1);
            let block_target = best_provider
                .get_block_number()
                .await
                .unwrap_or(0)
                + 1;

            let mut tx = TxEip1559 {
                to: mev_executor.into(),
                input: alloy::primitives::Bytes::from(call_data),
                gas_limit: config.max_gas as u128,
                max_fee_per_gas: (config.gas_price_wei * 2) as u128,
                chain_id: config.chain_id,
                nonce,
                ..Default::default()
            };

            let signed_tx = match s.sign_transaction(&mut tx).await {
                Ok(sig) => {
                    let signed = tx.into_signed(sig);
                    let envelope: TxEnvelope = signed.into();
                    envelope.encoded_2718()
                }
                Err(e) => {
                    warn!("❌ Transaction signing failed: {}", e);
                    continue;
                }
            };

            // Broadcast directly to Base mempool (MEV-Share does not exist on Base)
            match best_provider.send_raw_transaction(signed_tx.as_slice()).await {
                Ok(pending_tx) => {
                    info!(
                        "✅ Transaction BROADCASTED | user={:?} | tx_hash={:?} | nonce={} | block_target={}",
                        opp.user, pending_tx.tx_hash(), nonce, block_target
                    );
                    let mut cp = checkpoint.write().await;
                    cp.total_wins += 1;
                    if let Err(e) = cp.save(&config.checkpoint_path).await {
                        warn!("Failed to save checkpoint: {}", e);
                    }
                }
                Err(e) => {
                    warn!(
                        "❌ Transaction BROADCAST FAILED | user={:?} | nonce={} | error={}",
                        opp.user, nonce, e
                    );
                }
            }
        }

        Ok(())
    }
}
