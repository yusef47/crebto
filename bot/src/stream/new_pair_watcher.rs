use alloy::{
    rpc::types::eth::Log,
    primitives::{Address, B256, U256},
};
use eyre::{Result, eyre};
use tracing::info;

pub const UNISWAP_V3_POOL_CREATED_TOPIC: B256 = alloy::primitives::b256!("783debeafa1e8805bd1b483a6e765172f8ee93f65e54dddda1ef187a4d6194fa");

pub const AERODROME_POOL_CREATED_TOPIC: B256 = alloy::primitives::b256!("218a6d128cdcd0ae4e32171088781ccb541bb8cc3d89c175b28d8ce1e27e1475");

#[derive(Debug, Clone)]
pub struct NewPool {
    pub factory: Address,
    pub token0: Address,
    pub token1: Address,
    pub pool: Address,
    pub fee_bps: u32,
}

pub struct NewPairWatcher;

impl NewPairWatcher {
    pub fn parse_new_pool(log: &Log) -> Result<NewPool> {
        let topics = log.topics();
        if topics.is_empty() {
            return Err(eyre!("Empty topics"));
        }

        let topic0 = topics[0];

        if topic0 == UNISWAP_V3_POOL_CREATED_TOPIC {
            // Uniswap V3 PoolCreated:
            // topics[1]: token0 (indexed)
            // topics[2]: token1 (indexed)
            // topics[3]: fee (indexed)
            // data[0..32]: tickSpacing
            // data[32..64]: pool address
            let token0 = Address::from_word(topics[1]);
            let token1 = Address::from_word(topics[2]);
            let fee_word = topics[3];
            let fee_arr: [u8; 32] = fee_word.into();
            let fee_bps = u32::from_be_bytes([fee_arr[28], fee_arr[29], fee_arr[30], fee_arr[31]]);

            let data = log.data().data.as_ref();
            if data.len() < 64 {
                return Err(eyre!("Data too short for Uniswap PoolCreated"));
            }

            let pool_bytes = &data[44..64]; // Address is 20 bytes, padded to 32 bytes
            let pool = Address::from_slice(pool_bytes);

            info!("New Uniswap V3 Pool detected: {:?} (Tokens: {:?} / {:?}, Fee: {})", pool, token0, token1, fee_bps);

            Ok(NewPool {
                factory: log.address(),
                token0,
                token1,
                pool,
                fee_bps,
            })
        } else if topic0 == AERODROME_POOL_CREATED_TOPIC {
            // Aerodrome PoolCreated:
            // topics[1]: token0 (indexed)
            // topics[2]: token1 (indexed)
            // topics[3]: stable (indexed)
            // data[0..32]: pool
            // data[32..64]: index
            let token0 = Address::from_word(topics[1]);
            let token1 = Address::from_word(topics[2]);

            let data = log.data().data.as_ref();
            if data.len() < 64 {
                return Err(eyre!("Data too short for Aerodrome PoolCreated"));
            }

            let pool_bytes = &data[12..32];
            let pool = Address::from_slice(pool_bytes);

            info!("New Aerodrome Pool detected: {:?} (Tokens: {:?} / {:?})", pool, token0, token1);

            Ok(NewPool {
                factory: log.address(),
                token0,
                token1,
                pool,
                fee_bps: 30, // Default Aerodrome fee is 0.3% (30 bps)
            })
        } else {
            Err(eyre!("Unknown pool creation topic0"))
        }
    }
}
