use std::env;
use alloy::primitives::Address;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Config {
    /// Target chain ID (Base mainnet = 8453, Base Sepolia = 84532)
    pub chain_id: u64,
    /// Aave V3 Pool address on Base
    pub aave_pool: Address,
    /// Balancer V2 Vault address on Base
    pub balancer_vault: Address,
    /// MEVExecutor.sol deployment address
    pub executor_address: Address,
    /// Free RPC endpoints to rotate through
    pub rpc_urls: Vec<String>,
    /// MEV-Share endpoint
    pub mev_share_endpoint: String,
    /// Bot wallet private key (hex, no 0x prefix)
    pub bot_private_key: Option<String>,
    /// Polling interval in seconds (10s for Kaggle free tier)
    pub poll_interval_secs: u64,
    /// Max borrowers in watchlist
    pub max_borrowers: usize,
    /// Multicall batch size
    pub multicall_batch_size: usize,
    /// Health factor threshold to trigger liquidation (0.98 = 0.98 * 1e18)
    pub hf_threshold: u128,
    /// Min debt size in USD (1e8 = $1 USDC)
    pub min_debt_usd: u128,
    /// Max debt size in USD (micro-liquidation cap)
    pub max_debt_usd: u128,
    /// DRY_RUN mode: true = simulate only, false = submit bundles
    pub dry_run: bool,
    /// Path to save checkpoint JSON
    pub checkpoint_path: String,
    /// Path to load watchlist JSON
    pub watchlist_path: String,
    /// Base gas price estimate in wei (for simulation)
    pub gas_price_wei: u64,
    /// Max gas per liquidation tx
    pub max_gas: u64,
}

impl Config {
    pub fn from_env() -> Result<Self, eyre::Report> {
        let _ = dotenvy::dotenv();

        let chain_id = env::var("CHAIN_ID")
            .unwrap_or_else(|_| "8453".to_string())
            .parse::<u64>()
            .unwrap_or(8453);

        let aave_pool = env::var("AAVE_POOL")
            .unwrap_or_else(|_| "0xA238Dd80C22bdDf7D0EefB651440Ff9bA1D94454".to_string())
            .parse()
            .map_err(|_| eyre::eyre!("Invalid AAVE_POOL"))?;

        let balancer_vault = env::var("BALANCER_VAULT")
            .unwrap_or_else(|_| "0xBA12222222228d8Ba445958a75A0704d566BF2C8".to_string())
            .parse()
            .map_err(|_| eyre::eyre!("Invalid BALANCER_VAULT"))?;

        let executor_address = env::var("EXECUTOR_ADDRESS")
            .ok()
            .and_then(|s| Address::from_str(&s).ok())
            .unwrap_or_else(|| Address::from_str("0x0000000000000000000000000000000000000000").unwrap());

        let mev_share_endpoint = env::var("MEV_SHARE_ENDPOINT")
            .unwrap_or_else(|_| "https://mev-share-hilo.flashbots.net/".to_string());

        let bot_private_key = env::var("BOT_PRIVATE_KEY").ok();

        let rpc_urls: Vec<String> = env::var("RPC_URLS")
            .unwrap_or_else(|_| {
                "https://base-mainnet.g.alchemy.com/v2/demo,https://base.drpc.org,https://base-rpc.publicnode.com".to_string()
            })
            .split(',')
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .collect();

        let poll_interval_secs = env::var("POLL_INTERVAL_SECS")
            .unwrap_or_else(|_| "10".to_string())
            .parse::<u64>()
            .unwrap_or(10);

        let max_borrowers = env::var("MAX_BORROWERS")
            .unwrap_or_else(|_| "300".to_string())
            .parse::<usize>()
            .unwrap_or(300);

        let multicall_batch_size = env::var("MULTICALL_BATCH_SIZE")
            .unwrap_or_else(|_| "25".to_string())
            .parse::<usize>()
            .unwrap_or(25);

        let hf_threshold = env::var("HF_THRESHOLD")
            .unwrap_or_else(|_| "980000000000000000".to_string())
            .parse::<u128>()
            .unwrap_or(980_000_000_000_000_000u128);

        let min_debt_usd = env::var("MIN_DEBT_USD")
            .unwrap_or_else(|_| "100000000000".to_string()) // $1,000 * 1e8
            .parse::<u128>()
            .unwrap_or(100_000_000_000u128);

        let max_debt_usd = env::var("MAX_DEBT_USD")
            .unwrap_or_else(|_| "500000000000".to_string()) // $5,000 * 1e8
            .parse::<u128>()
            .unwrap_or(500_000_000_000u128);

        let dry_run = env::var("DRY_RUN")
            .unwrap_or_else(|_| "true".to_string())
            .parse::<bool>()
            .unwrap_or(true);

        let checkpoint_path = env::var("CHECKPOINT_PATH")
            .unwrap_or_else(|_| "/kaggle/working/bot_checkpoint.json".to_string());

        let watchlist_path = env::var("WATCHLIST_PATH")
            .unwrap_or_else(|_| "/kaggle/working/watchlist.json".to_string());

        let gas_price_wei = env::var("GAS_PRICE_WEI")
            .unwrap_or_else(|_| "500000000".to_string()) // 0.5 gwei
            .parse::<u64>()
            .unwrap_or(500_000_000);

        let max_gas = env::var("MAX_GAS")
            .unwrap_or_else(|_| "400000".to_string())
            .parse::<u64>()
            .unwrap_or(400_000);

        Ok(Self {
            chain_id,
            aave_pool,
            balancer_vault,
            executor_address,
            rpc_urls,
            mev_share_endpoint,
            bot_private_key,
            poll_interval_secs,
            max_borrowers,
            multicall_batch_size,
            hf_threshold,
            min_debt_usd,
            max_debt_usd,
            dry_run,
            checkpoint_path,
            watchlist_path,
            gas_price_wei,
            max_gas,
        })
    }
}
