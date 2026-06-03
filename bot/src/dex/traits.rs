use alloy::primitives::{Address, Bytes, U256};
use eyre::Result;

#[derive(Debug, Clone)]
pub struct PoolState {
    pub address: Address,
    pub token0: Address,
    pub token1: Address,
    pub reserve0: U256,
    pub reserve1: U256,
    pub fee_bps: u32,
    pub sqrt_price_x96: U256, // For V3
    pub liquidity: u128,      // For V3
    pub current_tick: i32,    // For V3
}

pub trait DexQuoter: Send + Sync {
    /// Estimates output amount for a given swap
    fn get_amount_out(
        &self,
        pool: &PoolState,
        token_in: Address,
        amount_in: U256,
    ) -> Result<U256>;

    /// Returns the optimal input amount to maximize arbitrage profit
    fn calculate_optimal_input(
        &self,
        pool_a: &PoolState,
        pool_b: &PoolState,
        max_input: U256,
    ) -> U256;

    /// Encodes swap instructions to be sent to the FlashArb contract
    fn encode_swap_step(
        &self,
        pool: &PoolState,
        token_in: Address,
        amount_in: U256,
        min_amount_out: U256,
        recipient: Address,
    ) -> Result<(Address, Bytes)>;
}
