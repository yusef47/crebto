use std::str::FromStr;
use alloy::{
    consensus::{TxEip1559, TxEnvelope, SignableTransaction},
    eips::eip2718::Encodable2718,
    network::TxSigner,
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
    mev_share::{MevBundle, MevShareSubmitter},
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
            info!("📦 Building bundle for liquidation: user={:?}", opp.user);

            // Step 1: Simulate via eth_call + StateOverride
            let best_provider = rpc.best_provider_owned();
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

            if !simulation_passed {
                warn!("❌ Simulation failed for user={:?}, skipping", opp.user);
                continue;
            }

            info!("✅ Simulation passed for user={:?}", opp.user);

            if config.dry_run {
                info!("🚫 DRY_RUN=true — bundle NOT submitted");
                let mut cp = checkpoint.write().await;
                cp.total_wins += 1;
                cp.total_profit_usd += 100.0; // placeholder
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

            let sig = s.sign_transaction(&mut tx).await?;
            let signed = tx.into_signed(sig);
            let envelope: TxEnvelope = signed.into();
            let signed_tx = envelope.encoded_2718();

            let bundle = MevBundle {
                signed_txs: vec![signed_tx],
                block_target,
                refund_address: s.address(),
                refund_percent: 100,
            };

            if let Err(e) = MevShareSubmitter::submit(&bundle, &config.mev_share_endpoint).await {
                warn!("Bundle submission failed: {}", e);
            }
        }

        Ok(())
    }
}
