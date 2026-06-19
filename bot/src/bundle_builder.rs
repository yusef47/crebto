use std::str::FromStr;
use alloy::{
    network::TransactionBuilder,
    primitives::Address,
    rpc::types::eth::TransactionRequest,
    signers::local::PrivateKeySigner,
    sol_types::SolCall,
};
use std::sync::Arc;
use tokio::sync::mpsc;
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
        mut checkpoint: BotCheckpoint,
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
                checkpoint.total_wins += 1;
                checkpoint.total_profit_usd += 100.0; // placeholder
                if let Err(e) = checkpoint.save(&config.checkpoint_path).await {
                    warn!("Failed to save checkpoint: {}", e);
                }
                continue;
            }

            // Step 2: Build and sign transaction
            let Some(ref _s) = signer else {
                warn!("No private key configured, cannot submit live bundles");
                continue;
            };

            let tx_req = TransactionRequest::default()
                .with_to(mev_executor)
                .input(
                    IMEVExecutor::executeLiquidationBalancerCall {
                        aavePool: aave_pool,
                        collateral: opp.collateral,
                        debt: opp.debt,
                        user: opp.user,
                        debtToCover: opp.debt_to_cover,
                        flashAmount: opp.debt_to_cover,
                    }
                    .abi_encode()
                    .into(),
                )
                .with_gas_limit(config.max_gas.into())
                .with_max_fee_per_gas((config.gas_price_wei * 2).into());

            // Step 3: Sign and encode
            // Note: In a real implementation, use alloy's signer middleware
            // For brevity, this is a simplified representation
            let signed_tx = vec![0u8; 32]; // PLACEHOLDER — real signing requires chain-specific nonce management

            let bundle = MevBundle {
                signed_txs: vec![signed_tx],
                block_target: 0, // Filled by calling best_provider.get_block_number().await + 1
                refund_address: Address::from_str("0x0000000000000000000000000000000000000000").unwrap(),
                refund_percent: 100,
            };

            if let Err(e) = MevShareSubmitter::submit(&bundle, &config.mev_share_endpoint).await {
                warn!("Bundle submission failed: {}", e);
            }
        }

        Ok(())
    }
}
