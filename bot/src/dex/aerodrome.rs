use crate::dex::traits::{DexQuoter, PoolState};
use alloy::{
    primitives::{Address, Bytes, U256},
    sol,
};
use eyre::Result;

// ABI representation for Aerodrome router calls
sol! {
    struct Route {
        address from;
        address to;
        bool stable;
        address factory;
    }

    interface IAeroRouter {
        function swapExactTokensForTokens(
            uint256 amountIn,
            uint256 amountOutMin,
            Route[] calldata routes,
            address to,
            uint256 deadline
        ) external returns (uint256[] memory amounts);
    }
}

pub struct AerodromeQuoter {
    pub router_address: Address,
    pub factory_address: Address,
    pub is_stable: bool,
}

impl AerodromeQuoter {
    pub fn new(router_address: Address, factory_address: Address, is_stable: bool) -> Self {
        Self {
            router_address,
            factory_address,
            is_stable,
        }
    }
}

impl DexQuoter for AerodromeQuoter {
    fn get_amount_out(
        &self,
        pool: &PoolState,
        token_in: Address,
        amount_in: U256,
    ) -> Result<U256> {
        if amount_in.is_zero() {
            return Ok(U256::ZERO);
        }

        let (reserve_in, reserve_out) = if token_in == pool.token0 {
            (pool.reserve0, pool.reserve1)
        } else {
            (pool.reserve1, pool.reserve0)
        };

        if reserve_in.is_zero() || reserve_out.is_zero() {
            return Ok(U256::ZERO);
        }

        // Aerodrome Fee: dynamic, typically around 0.3% (30 bps) or less.
        let fee_bps = pool.fee_bps;
        let amount_in_after_fee = (amount_in * U256::from(10000 - fee_bps)) / U256::from(10000);

        if self.is_stable {
            // Stable AMM formula: x^3*y + y^3*x = k
            // For approximation, we use a Newton-Raphson-like iterative solver
            // Let f(y) = x^3*y + y^3*x - k
            // To simplify off-chain, we can approximate it or use binary search
            let x = reserve_in + amount_in_after_fee;
            let mut low = U256::ZERO;
            let mut high = reserve_out;
            let mut best_y = U256::ZERO;

            // Target k-value approximation
            let k = (reserve_in * reserve_in * reserve_in * reserve_out) + 
                    (reserve_out * reserve_out * reserve_out * reserve_in);

            for _ in 0..20 {
                let mid = (low + high) / U256::from(2);
                let current_k = (x * x * x * mid) + (mid * mid * mid * x);
                if current_k < k {
                    low = mid + U256::from(1);
                } else {
                    best_y = mid;
                    if mid.is_zero() {
                        break;
                    }
                    high = mid - U256::from(1);
                }
            }
            
            // The amount out is reserve_out - best_y
            if reserve_out > best_y {
                Ok(reserve_out - best_y)
            } else {
                Ok(U256::ZERO)
            }
        } else {
            // Volatile pool: Constant Product formula (xy=k)
            let denominator = reserve_in + amount_in_after_fee;
            let amount_out = (amount_in_after_fee * reserve_out) / denominator;
            Ok(amount_out)
        }
    }

    fn calculate_optimal_input(
        &self,
        pool_a: &PoolState,
        pool_b: &PoolState,
        max_input: U256,
    ) -> U256 {
        // Optimal amount using binary search
        let mut low = U256::ZERO;
        let mut high = max_input;
        let mut best_input = U256::ZERO;
        let mut max_profit = U256::ZERO;

        for _ in 0..15 {
            let mid = (low + high) / U256::from(2);
            if mid.is_zero() {
                break;
            }

            let amount_out_a = self.get_amount_out(pool_a, pool_a.token0, mid).unwrap_or(U256::ZERO);
            let amount_out_b = self.get_amount_out(pool_b, pool_a.token1, amount_out_a).unwrap_or(U256::ZERO);

            if amount_out_b > mid {
                let profit = amount_out_b - mid;
                if profit > max_profit {
                    max_profit = profit;
                    best_input = mid;
                }
                low = mid + U256::from(1);
            } else {
                high = mid - U256::from(1);
            }
        }

        best_input
    }

    fn encode_swap_step(
        &self,
        pool: &PoolState,
        token_in: Address,
        amount_in: U256,
        min_amount_out: U256,
        recipient: Address,
    ) -> Result<(Address, Bytes)> {
        let token_out = if token_in == pool.token0 { pool.token1 } else { pool.token0 };

        let route = Route {
            from: token_in,
            to: token_out,
            stable: self.is_stable,
            factory: self.factory_address,
        };

        let routes = vec![route];

        // 20 minute deadline from execution (can be any far future timestamp)
        let deadline = U256::from(999999999999999u64);

        let call_data = IAeroRouter::swapExactTokensForTokensCall {
            amountIn: amount_in,
            amountOutMin: min_amount_out,
            routes,
            to: recipient,
            deadline,
        }
        .abi_encode();

        Ok((self.router_address, Bytes::from(call_data)))
    }
}
