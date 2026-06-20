use alloy::{
    network::{Ethereum, TransactionBuilder},
    primitives::{Address, U256},
    providers::Provider,
    rpc::types::eth::TransactionRequest,
    sol_types::SolCall,
};
use reqwest::Client;
use tracing::warn;

use crate::contracts::IMEVExecutor;

/// Lightweight simulation using eth_call + StateOverride.
/// No Anvil fork = no RAM bloat on Kaggle.
pub async fn simulate_liquidation<P>(
    provider: &P,
    mev_executor: Address,
    aave_pool: Address,
    user: Address,
    collateral: Address,
    debt: Address,
    debt_to_cover: U256,
    flash_amount: U256,
) -> eyre::Result<bool>
where
    P: Provider<alloy::transports::http::Http<Client>, Ethereum>,
{
    let call_data = IMEVExecutor::executeLiquidationBalancerCall {
        aavePool: aave_pool,
        collateral,
        debt,
        user,
        debtToCover: debt_to_cover,
        flashAmount: flash_amount,
        minAmountOut: U256::ZERO,
    }
    .abi_encode();

    let tx = TransactionRequest::default()
        .with_to(mev_executor)
        .with_from(mev_executor) // simulate as if executor calls itself
        .with_value(U256::ZERO)
        .with_input(alloy::primitives::Bytes::from(call_data));

    match provider.call(&tx).await {
        Ok(_) => Ok(true),
        Err(e) => {
            warn!("Simulation failed: {}", e);
            Ok(false)
        }
    }
}
