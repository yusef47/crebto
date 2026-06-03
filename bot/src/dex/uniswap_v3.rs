use crate::dex::traits::{DexQuoter, PoolState};
use alloy::{
    primitives::{Address, Bytes, U256},
    sol,
    sol_types::SolCall,
};
use eyre::Result;

// ABI representation for Uniswap V3 router calls
sol! {
    struct ExactInputSingleParams {
        address tokenIn;
        address tokenOut;
        uint24 fee;
        address recipient;
        uint256 amountIn;
        uint256 amountOutMinimum;
        uint160 sqrtPriceLimitX96;
    }

    interface IUniswapV3Router {
        function exactInputSingle(ExactInputSingleParams calldata params) external payable returns (uint256 amountOut);
    }
}

pub struct UniswapV3Quoter {
    pub router_address: Address,
}

impl UniswapV3Quoter {
    pub fn new(router_address: Address) -> Self {
        Self { router_address }
    }
}

impl DexQuoter for UniswapV3Quoter {
    fn get_amount_out(
        &self,
        pool: &PoolState,
        token_in: Address,
        amount_in: U256,
    ) -> Result<U256> {
        if amount_in.is_zero() {
            return Ok(U256::ZERO);
        }

        // Simplified Uniswap V3 single-tick swap simulation
        // In Uniswap V3, delta_y = L * delta_sqrt_p
        // And delta_x = L * delta_(1/sqrt_p)
        let L = U256::from(pool.liquidity);
        if L.is_zero() {
            return Ok(U256::ZERO);
        }

        // Apply fee
        let fee_multiplier = U256::from(1000000 - pool.fee_bps); // V3 fee is in parts per million (e.g. 3000 bps = 0.3%)
        let amount_in_after_fee = (amount_in * fee_multiplier) / U256::from(1000000);

        let is_token0 = token_in == pool.token0;
        
        if is_token0 {
            // Selling token0 for token1 (x for y)
            // sqrtPriceX96 = sqrtPrice * 2^96
            // amount_out = amount_in_after_fee * sqrtPriceX96 / 2^96 (roughly)
            let q96 = U256::from(1) << 96;
            let price: U256 = (pool.sqrt_price_x96 * pool.sqrt_price_x96) / q96;
            let amount_out = (amount_in_after_fee * price) / q96;
            Ok(amount_out)
        } else {
            // Selling token1 for token0 (y for x)
            // amount_out = amount_in_after_fee * 2^96 / sqrtPriceX96 (roughly)
            let q96 = U256::from(1) << 96;
            let price: U256 = (pool.sqrt_price_x96 * pool.sqrt_price_x96) / q96;
            if price.is_zero() {
                return Ok(U256::ZERO);
            }
            let amount_out = (amount_in_after_fee * q96 * q96) / (price * q96);
            Ok(amount_out)
        }
    }

    fn calculate_optimal_input(
        &self,
        pool_a: &PoolState,
        pool_b: &PoolState,
        max_input: U256,
    ) -> U256 {
        // Optimal amount for V3 can be approximated using a small binary search
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

        let params = ExactInputSingleParams {
            tokenIn: token_in,
            tokenOut: token_out,
            fee: alloy::primitives::aliases::U24::from(pool.fee_bps), // e.g. 500, 3000, 10000
            recipient,
            amountIn: amount_in,
            amountOutMinimum: min_amount_out,
            sqrtPriceLimitX96: alloy::primitives::aliases::U160::ZERO,
        };

        let call_data = IUniswapV3Router::exactInputSingleCall { params }.abi_encode();

        Ok((self.router_address, Bytes::from(call_data)))
    }
}
