use crate::dex::traits::{DexQuoter, PoolState};
use crate::executor::TxBuilder;
use crate::simulator::{SimulationRequest, SizedSimulationRequest};
use crate::strategy::pool_tracker::ArbOpportunity;
use alloy::primitives::{Address, U256};
use eyre::{eyre, Result};

#[derive(Debug, Clone)]
pub struct CandidateBuildConfig {
    pub caller: Address,
    pub contract_address: Address,
    pub gas_limit: u64,
    pub gas_price: U256,
    pub slippage_bps: u32,
    pub min_profit_wei: U256,
    pub min_profit_by_asset: Vec<(Address, U256)>,
    pub native_token_units_by_asset: Vec<(Address, U256)>,
    pub flash_fee_bps: u32,
    pub flash_assets: Vec<Address>,
}

#[derive(Debug, Clone)]
pub struct ExecutionCandidateMeta {
    pub borrow_asset: Address,
    pub first_pool: Address,
    pub second_pool: Address,
    pub amount_in: U256,
    pub expected_profit_wei: U256,
    pub min_profit_wei: U256,
}

pub struct CandidateBuilder;

impl CandidateBuilder {
    pub fn build_size_grid(
        opp: &ArbOpportunity,
        pool_a: &PoolState,
        quoter_a: &dyn DexQuoter,
        pool_b: &PoolState,
        quoter_b: &dyn DexQuoter,
        tx_builder: &TxBuilder,
        config: &CandidateBuildConfig,
        probe_amounts: &[(Address, U256)],
    ) -> Result<Vec<(ExecutionCandidateMeta, SizedSimulationRequest)>> {
        let shared = shared_pair(pool_a, pool_b)
            .ok_or_else(|| eyre!("Candidate pools must contain the same token pair"))?;

        let mut candidates = Vec::new();
        let directions = [
            (pool_a, quoter_a, pool_b, quoter_b, shared.0, shared.1, &opp.dex_a, &opp.dex_b),
            (pool_a, quoter_a, pool_b, quoter_b, shared.1, shared.0, &opp.dex_a, &opp.dex_b),
            (pool_b, quoter_b, pool_a, quoter_a, shared.0, shared.1, &opp.dex_b, &opp.dex_a),
            (pool_b, quoter_b, pool_a, quoter_a, shared.1, shared.0, &opp.dex_b, &opp.dex_a),
        ];

        for (
            first_pool,
            first_quoter,
            second_pool,
            second_quoter,
            borrow_asset,
            intermediate_asset,
            first_dex,
            second_dex,
        ) in directions {
            if !config.flash_assets.contains(&borrow_asset) {
                continue;
            }
            let min_profit_for_borrow_asset = config
                .min_profit_by_asset
                .iter()
                .find_map(|(asset, min_profit)| {
                    if *asset == borrow_asset {
                        Some(*min_profit)
                    } else {
                        None
                    }
                })
                .unwrap_or(config.min_profit_wei);
            let native_token_units_in_profit_asset = config
                .native_token_units_by_asset
                .iter()
                .find_map(|(asset, units)| if *asset == borrow_asset { Some(*units) } else { None })
                .unwrap_or(U256::from(1_000_000_000_000_000_000u128));

            for (_, amount_in) in probe_amounts
                .iter()
                .copied()
                .filter(|(asset, amount)| *asset == borrow_asset && !amount.is_zero())
            {
                let first_out = first_quoter.get_amount_out(first_pool, borrow_asset, amount_in)?;
                if first_out.is_zero() {
                    continue;
                }

                let final_out =
                    second_quoter.get_amount_out(second_pool, intermediate_asset, first_out)?;
                let flash_fee = (amount_in * U256::from(config.flash_fee_bps)) / U256::from(10_000);
                let repayment = amount_in + flash_fee;
                if final_out <= repayment {
                    continue;
                }

                let expected_profit = final_out - repayment;
                if expected_profit < min_profit_for_borrow_asset {
                    continue;
                }

                let min_first_out = apply_slippage_floor(first_out, config.slippage_bps);
                let min_final_out = repayment + min_profit_for_borrow_asset;

                let first_step = first_quoter.encode_swap_step(
                    first_pool,
                    borrow_asset,
                    amount_in,
                    min_first_out,
                    config.contract_address,
                )?;

                let second_step = second_quoter.encode_swap_step(
                    second_pool,
                    intermediate_asset,
                    first_out,
                    min_final_out,
                    config.contract_address,
                )?;

                let min_profit = min_profit_for_borrow_asset.max(apply_slippage_floor(
                    expected_profit,
                    config.slippage_bps,
                ));

                let call_data = tx_builder.encode_arbitrage_call(
                    borrow_asset,
                    amount_in,
                    min_profit,
                    vec![first_step, second_step],
                );

                let request = SimulationRequest {
                    caller: config.caller,
                    contract_address: config.contract_address,
                    call_data,
                    gas_limit: config.gas_limit,
                    gas_price: config.gas_price,
                };

                let meta = ExecutionCandidateMeta {
                    borrow_asset,
                    first_pool: first_pool.address,
                    second_pool: second_pool.address,
                    amount_in,
                    expected_profit_wei: expected_profit,
                    min_profit_wei: min_profit,
                };

                tracing::debug!(
                    "Built candidate {} -> {} amount={} expected_profit={}",
                    first_dex,
                    second_dex,
                    amount_in,
                    expected_profit
                );

                candidates.push((
                    meta,
                    SizedSimulationRequest {
                        amount_in,
                        expected_profit_wei: expected_profit,
                        native_token_units_in_profit_asset,
                        request,
                    },
                ));
            }
        }

        Ok(candidates)
    }
}

fn apply_slippage_floor(amount: U256, slippage_bps: u32) -> U256 {
    let keep_bps = 10_000u32.saturating_sub(slippage_bps.min(10_000));
    (amount * U256::from(keep_bps)) / U256::from(10_000)
}

fn shared_pair(pool_a: &PoolState, pool_b: &PoolState) -> Option<(Address, Address)> {
    let same_order = pool_a.token0 == pool_b.token0 && pool_a.token1 == pool_b.token1;
    let reversed = pool_a.token0 == pool_b.token1 && pool_a.token1 == pool_b.token0;

    if same_order || reversed {
        Some((pool_a.token0, pool_a.token1))
    } else {
        None
    }
}
