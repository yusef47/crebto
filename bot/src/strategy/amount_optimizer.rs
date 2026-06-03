use crate::dex::traits::{DexQuoter, PoolState};
use crate::dex::uniswap_v3::UniswapV3Quoter;
use crate::dex::aerodrome::AerodromeQuoter;
use crate::strategy::path_finder::ArbPath;
use alloy::primitives::{Address, U256};
use eyre::Result;

pub struct AmountOptimizer;

impl AmountOptimizer {
    /// Evaluates the path with a specific input amount and returns estimated output
    pub fn evaluate_amount(
        path: &ArbPath,
        amount_in: U256,
        uni_quoter: &UniswapV3Quoter,
        aero_quoter: &AerodromeQuoter,
    ) -> U256 {
        let mut current_amount = amount_in;

        for hop in &path.hops {
            let quoter: &dyn DexQuoter = if hop.dex_type == "uniswap_v3" {
                uni_quoter
            } else {
                aero_quoter
            };

            match quoter.get_amount_out(&hop.pool, hop.token_in, current_amount) {
                Ok(out) => current_amount = out,
                Err(_) => return U256::ZERO,
            }
        }

        current_amount
    }

    /// Finds the optimal amount using binary search to maximize profit
    pub fn optimize_input_amount(
        path: &ArbPath,
        max_limit: U256,
        uni_quoter: &UniswapV3Quoter,
        aero_quoter: &AerodromeQuoter,
    ) -> (U256, U256) {
        let mut low = U256::from(10_000_000u64); // Min borrow (e.g. 10 USDC or WETH equivalent)
        let mut high = max_limit;
        let mut best_amount = U256::ZERO;
        let mut max_profit = U256::ZERO;

        for _ in 0..20 {
            if low >= high {
                break;
            }

            let mid = (low + high) / U256::from(2);
            let estimated_out = Self::evaluate_amount(path, mid, uni_quoter, aero_quoter);
            
            // Profit calculation: output - input - Aave flash loan fee (0.05%)
            let aave_fee = (mid * U256::from(5)) / U256::from(10000);
            let total_cost = mid + aave_fee;

            if estimated_out > total_cost {
                let profit = estimated_out - total_cost;
                if profit > max_profit {
                    max_profit = profit;
                    best_amount = mid;
                }
                // Try higher amounts to see if depth allows more profit
                low = mid + U256::from(1);
            } else {
                // If not profitable, decrease the amount size
                high = mid - U256::from(1);
            }
        }

        (best_amount, max_profit)
    }
}
