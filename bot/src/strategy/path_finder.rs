use crate::dex::traits::PoolState;
use alloy::primitives::Address;

#[derive(Debug, Clone)]
pub struct Hop {
    pub pool: PoolState,
    pub dex_type: String, // "uniswap_v3", "aerodrome", "sushiswap_v3"
    pub token_in: Address,
    pub token_out: Address,
}

#[derive(Debug, Clone)]
pub struct ArbPath {
    pub hops: Vec<Hop>,
}

pub struct PathFinder;

impl PathFinder {
    /// Finds all possible direct loops between two pools sharing the same token pair
    pub fn find_direct_paths(pools: &[PoolState]) -> Vec<ArbPath> {
        let mut paths = Vec::new();
        let len = pools.len();

        for i in 0..len {
            for j in 0..len {
                if i == j {
                    continue;
                }

                let pool_a = &pools[i];
                let pool_b = &pools[j];

                // Check if they share the exact same token pair
                let same_pair = (pool_a.token0 == pool_b.token0 && pool_a.token1 == pool_b.token1) ||
                                (pool_a.token0 == pool_b.token1 && pool_a.token1 == pool_b.token0);

                if same_pair {
                    // Create path: pool_a (token0 -> token1) -> pool_b (token1 -> token0)
                    let hop1 = Hop {
                        pool: pool_a.clone(),
                        dex_type: Self::determine_dex_type(pool_a),
                        token_in: pool_a.token0,
                        token_out: pool_a.token1,
                    };
                    let hop2 = Hop {
                        pool: pool_b.clone(),
                        dex_type: Self::determine_dex_type(pool_b),
                        token_in: pool_a.token1,
                        token_out: pool_a.token0,
                    };

                    paths.push(ArbPath {
                        hops: vec![hop1, hop2],
                    });
                }
            }
        }

        paths
    }

    fn determine_dex_type(pool: &PoolState) -> String {
        // Mock classification based on address or properties
        if pool.sqrt_price_x96 > alloy::primitives::U256::ZERO {
            "uniswap_v3".to_string()
        } else {
            "aerodrome".to_string()
        }
    }
}
