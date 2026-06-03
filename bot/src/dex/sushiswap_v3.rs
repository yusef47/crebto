use crate::dex::traits::{DexQuoter, PoolState};
use crate::dex::uniswap_v3::UniswapV3Quoter;
use alloy::primitives::{Address, Bytes, U256};
use eyre::Result;

pub struct SushiSwapV3Quoter {
    quoter: UniswapV3Quoter,
}

impl SushiSwapV3Quoter {
    pub fn new(router_address: Address) -> Self {
        Self {
            quoter: UniswapV3Quoter::new(router_address),
        }
    }
}

impl DexQuoter for SushiSwapV3Quoter {
    fn get_amount_out(
        &self,
        pool: &PoolState,
        token_in: Address,
        amount_in: U256,
    ) -> Result<U256> {
        self.quoter.get_amount_out(pool, token_in, amount_in)
    }

    fn calculate_optimal_input(
        &self,
        pool_a: &PoolState,
        pool_b: &PoolState,
        max_input: U256,
    ) -> U256 {
        self.quoter.calculate_optimal_input(pool_a, pool_b, max_input)
    }

    fn encode_swap_step(
        &self,
        pool: &PoolState,
        token_in: Address,
        amount_in: U256,
        min_amount_out: U256,
        recipient: Address,
    ) -> Result<(Address, Bytes)> {
        self.quoter.encode_swap_step(pool, token_in, amount_in, min_amount_out, recipient)
    }
}
