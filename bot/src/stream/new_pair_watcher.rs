use alloy::{
    rpc::types::eth::Log,
    primitives::{Address, B256},
};
use eyre::{Result, eyre};
use tracing::info;

use crate::stream::ws_listener::{
    AERODROME_SLIPSTREAM_FACTORY, AERODROME_V2_FACTORY, UNISWAP_V3_FACTORY,
};

pub const UNISWAP_V3_POOL_CREATED_TOPIC: B256 = alloy::primitives::b256!("783cca1c0412dd0d695e784568c96da2e9c22ff989357a2e8b1d9b2b4e6b7118");

pub const AERODROME_POOL_CREATED_TOPIC: B256 = alloy::primitives::b256!("2128d88d14c80cb081c1252a5acff7a264671bf199ce226b53788fb26065005e");

pub const AERODROME_SLIPSTREAM_POOL_CREATED_TOPIC: B256 = alloy::primitives::b256!("ab0d57f0df537bb25e80245ef7748fa62353808c54d6e528a9dd20887aed9ac2");

#[derive(Debug, Clone)]
pub struct NewPool {
    pub factory: Address,
    pub token0: Address,
    pub token1: Address,
    pub pool: Address,
    pub fee_bps: u32,
    pub tick_spacing: Option<i32>,
    pub dex_name: String,
}

pub struct NewPairWatcher;

impl NewPairWatcher {
    pub fn is_supported_factory(address: Address) -> bool {
        address == UNISWAP_V3_FACTORY
            || address == AERODROME_V2_FACTORY
            || address == AERODROME_SLIPSTREAM_FACTORY
    }

    pub fn parse_new_pool(log: &Log) -> Result<NewPool> {
        let topics = log.topics();
        if topics.is_empty() {
            return Err(eyre!("Empty topics"));
        }

        let topic0 = topics[0];

        if log.address() == UNISWAP_V3_FACTORY && topic0 == UNISWAP_V3_POOL_CREATED_TOPIC {
            if topics.len() < 4 {
                return Err(eyre!("Uniswap PoolCreated missing indexed topics"));
            }

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
                tick_spacing: None,
                dex_name: "uniswap_v3".to_string(),
            })
        } else if log.address() == AERODROME_V2_FACTORY && topic0 == AERODROME_POOL_CREATED_TOPIC {
            if topics.len() < 4 {
                return Err(eyre!("Aerodrome V2 PoolCreated missing indexed topics"));
            }

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
                tick_spacing: None,
                dex_name: "aerodrome_v2".to_string(),
            })
        } else if log.address() == AERODROME_SLIPSTREAM_FACTORY && topic0 == AERODROME_SLIPSTREAM_POOL_CREATED_TOPIC {
            if topics.len() < 4 {
                return Err(eyre!("Aerodrome Slipstream PoolCreated missing indexed topics"));
            }

            // Aerodrome Slipstream CLFactory PoolCreated:
            // topics[1]: token0 (indexed)
            // topics[2]: token1 (indexed)
            // topics[3]: tickSpacing (indexed int24)
            // data[0..32]: pool address
            let token0 = Address::from_word(topics[1]);
            let token1 = Address::from_word(topics[2]);

            let tick_word: [u8; 32] = topics[3].into();
            let tick_spacing = i32::from_be_bytes([
                if tick_word[29] & 0x80 != 0 { 0xff } else { 0x00 },
                tick_word[29],
                tick_word[30],
                tick_word[31],
            ]);

            let data = log.data().data.as_ref();
            if data.len() < 32 {
                return Err(eyre!("Data too short for Aerodrome Slipstream PoolCreated"));
            }

            let pool = Address::from_slice(&data[12..32]);
            let fee_bps = match tick_spacing {
                1 => 100,
                50 | 100 => 500,
                200 => 3000,
                2000 => 10000,
                _ => 3000,
            };

            info!("New Aerodrome Slipstream Pool detected: {:?} (Tokens: {:?} / {:?}, Tick spacing: {}, Fee units: {})", pool, token0, token1, tick_spacing, fee_bps);

            Ok(NewPool {
                factory: log.address(),
                token0,
                token1,
                pool,
                fee_bps,
                tick_spacing: Some(tick_spacing),
                dex_name: "aerodrome_cl".to_string(),
            })
        } else {
            Err(eyre!("Unknown pool creation topic0"))
        }
    }
}
