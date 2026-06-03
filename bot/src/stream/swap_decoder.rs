use crate::stream::ws_listener::{UNISWAP_V3_SWAP_TOPIC, AERODROME_V2_SWAP_TOPIC};
use alloy::{
    rpc::types::eth::Log,
    primitives::{Address, B256, U256},
};
use eyre::{Result, eyre};

#[derive(Debug, Clone)]
pub enum DecodedSwap {
    UniswapV3 {
        pool: Address,
        amount0: U256,
        amount1: U256,
        is_amount0_negative: bool,
        is_amount1_negative: bool,
        sqrt_price_x96: U256,
        liquidity: u128,
        tick: i32,
    },
    Aerodrome {
        pool: Address,
        amount0_in: U256,
        amount1_in: U256,
        amount0_out: U256,
        amount1_out: U256,
    },
}

pub struct SwapDecoder;

impl SwapDecoder {
    pub fn decode(log: &Log) -> Result<DecodedSwap> {
        let topics = log.topics();
        if topics.is_empty() {
            return Err(eyre!("Empty topics in log"));
        }

        let topic0 = topics[0];
        let pool = log.address();

        if topic0 == UNISWAP_V3_SWAP_TOPIC {
            // Uniswap V3 Swap event layout in log.data (all fields are 32-byte padded):
            // data[0..32]:   amount0 (int256)
            // data[32..64]:  amount1 (int256)
            // data[64..96]:  sqrtPriceX96 (uint160)
            // data[96..128]: liquidity (uint128)
            // data[128..160]: tick (int24)
            let data = log.data().data.as_ref();
            if data.len() < 160 {
                return Err(eyre!("USV3 log data too short: {}", data.len()));
            }

            // Read signed int256 values
            let amount0_raw = U256::from_be_slice(&data[0..32]);
            let amount1_raw = U256::from_be_slice(&data[32..64]);

            // Check sign bit (MSB) for signed integers
            let is_amount0_negative = (amount0_raw.as_limbs()[3] & (1 << 63)) != 0;
            let is_amount1_negative = (amount1_raw.as_limbs()[3] & (1 << 63)) != 0;

            // Abs values if negative (two's complement)
            let amount0 = if is_amount0_negative {
                (!amount0_raw).checked_add(U256::from(1)).unwrap_or(U256::ZERO)
            } else {
                amount0_raw
            };

            let amount1 = if is_amount1_negative {
                (!amount1_raw).checked_add(U256::from(1)).unwrap_or(U256::ZERO)
            } else {
                amount1_raw
            };

            let sqrt_price_x96 = U256::from_be_slice(&data[64..96]);
            
            let liquidity_bytes = &data[96..128];
            let mut liquidity_arr = [0u8; 16];
            liquidity_arr.copy_from_slice(&liquidity_bytes[16..32]);
            let liquidity = u128::from_be_bytes(liquidity_arr);

            let tick_bytes = &data[128..160];
            let mut tick_arr = [0u8; 4];
            tick_arr.copy_from_slice(&tick_bytes[28..32]);
            let tick = i32::from_be_bytes(tick_arr);

            Ok(DecodedSwap::UniswapV3 {
                pool,
                amount0,
                amount1,
                is_amount0_negative,
                is_amount1_negative,
                sqrt_price_x96,
                liquidity,
                tick,
            })
        } else if topic0 == AERODROME_V2_SWAP_TOPIC {
            // Aerodrome Standard Swap event:
            // data[0..32]:   amount0In (uint256)
            // data[32..64]:  amount1In (uint256)
            // data[64..96]:  amount0Out (uint256)
            // data[96..128]: amount1Out (uint256)
            let data = log.data().data.as_ref();
            if data.len() < 128 {
                return Err(eyre!("Aerodrome log data too short: {}", data.len()));
            }

            let amount0_in = U256::from_be_slice(&data[0..32]);
            let amount1_in = U256::from_be_slice(&data[32..64]);
            let amount0_out = U256::from_be_slice(&data[64..96]);
            let amount1_out = U256::from_be_slice(&data[96..128]);

            Ok(DecodedSwap::Aerodrome {
                pool,
                amount0_in,
                amount1_in,
                amount0_out,
                amount1_out,
            })
        } else {
            Err(eyre!("Unknown topic0 signature"))
        }
    }
}
