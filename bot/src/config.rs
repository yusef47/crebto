use std::env;
use alloy::primitives::Address;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Config {
    pub alchemy_wss: String,
    pub alchemy_http: String,
    pub dry_run: bool,
    pub private_key: Option<String>,
    pub executor_address: Option<Address>,
    pub contract_address: Option<Address>,
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub max_gas_price_gwei: u64,
    pub min_eth_balance: f64,
    pub max_loss_per_hour_usd: f64,
    pub max_consecutive_failures: u32,
    pub min_profit_usd: f64,
    pub require_simulation: bool,
    pub enable_live_send: bool,
    pub uniswap_v3_router: Option<Address>,
    pub aerodrome_router: Option<Address>,
    pub aerodrome_slipstream_router: Option<Address>,
    pub aerodrome_factory: Option<Address>,
    pub execution_gas_limit: u64,
    pub slippage_bps: u32,
    pub probe_sizes_usd: Vec<f64>,
    // v0.6: Safety & live trading controls
    pub min_liquidity_usd: f64,
    pub max_tax_bps: u32,
    pub max_daily_loss_usd: f64,
    pub max_trades_per_hour: u32,
}

impl Config {
    pub fn load_from_env() -> Result<Self, eyre::Report> {
        // Load .env file if it exists (local dev)
        let _ = dotenvy::dotenv();

        // DRY_RUN mode: if true, bot will only monitor and simulate, never send real TXs
        let dry_run = env::var("DRY_RUN")
            .unwrap_or_else(|_| "true".to_string())
            .parse::<bool>()
            .unwrap_or(true);

        let alchemy_wss = env::var("ALCHEMY_WSS")
            .map_err(|_| eyre::eyre!("ALCHEMY_WSS must be set in environment"))?;

        let alchemy_http = env::var("ALCHEMY_HTTP")
            .unwrap_or_else(|_| alchemy_wss.replace("wss://", "https://").replace("/ws/", "/"));

        // In DRY_RUN mode, private key and contract address are optional
        let private_key = env::var("PRIVATE_KEY").ok();

        let executor_address = env::var("EXECUTOR_ADDRESS")
            .ok()
            .and_then(|s| Address::from_str(&s).ok());

        let contract_address = env::var("CONTRACT_ADDRESS")
            .ok()
            .and_then(|s| Address::from_str(&s).ok());

        let telegram_bot_token = env::var("TELEGRAM_BOT_TOKEN").ok();
        let telegram_chat_id = env::var("TELEGRAM_CHAT_ID").ok();

        let max_gas_price_gwei = env::var("MAX_GAS_PRICE_GWEI")
            .unwrap_or_else(|_| "100".to_string())
            .parse::<u64>()
            .unwrap_or(100);

        let min_eth_balance = env::var("MIN_ETH_BALANCE")
            .unwrap_or_else(|_| "0.005".to_string())
            .parse::<f64>()
            .unwrap_or(0.005);

        let max_loss_per_hour_usd = env::var("MAX_LOSS_PER_HOUR_USD")
            .unwrap_or_else(|_| "5.0".to_string())
            .parse::<f64>()
            .unwrap_or(5.0);

        let max_consecutive_failures = env::var("MAX_CONSECUTIVE_FAILURES")
            .unwrap_or_else(|_| "50".to_string())
            .parse::<u32>()
            .unwrap_or(50);

        let min_profit_usd = env::var("MIN_PROFIT_USD")
            .unwrap_or_else(|_| "0.20".to_string())
            .parse::<f64>()
            .unwrap_or(0.20);

        let require_simulation = env::var("REQUIRE_SIMULATION")
            .unwrap_or_else(|_| "true".to_string())
            .parse::<bool>()
            .unwrap_or(true);

        let enable_live_send = env::var("ENABLE_LIVE_SEND")
            .unwrap_or_else(|_| "false".to_string())
            .parse::<bool>()
            .unwrap_or(false);

        let uniswap_v3_router = env::var("UNISWAP_V3_ROUTER")
            .ok()
            .and_then(|s| Address::from_str(&s).ok());

        let aerodrome_router = env::var("AERODROME_ROUTER")
            .ok()
            .and_then(|s| Address::from_str(&s).ok());

        let aerodrome_slipstream_router = env::var("AERODROME_SLIPSTREAM_ROUTER")
            .ok()
            .and_then(|s| Address::from_str(&s).ok())
            .or(aerodrome_router);

        let aerodrome_factory = env::var("AERODROME_FACTORY")
            .ok()
            .and_then(|s| Address::from_str(&s).ok());

        let execution_gas_limit = env::var("EXECUTION_GAS_LIMIT")
            .unwrap_or_else(|_| "600000".to_string())
            .parse::<u64>()
            .unwrap_or(600_000);

        let slippage_bps = env::var("SLIPPAGE_BPS")
            .unwrap_or_else(|_| "15".to_string())
            .parse::<u32>()
            .unwrap_or(15);

        let probe_sizes_usd = env::var("PROBE_SIZES_USD")
            .unwrap_or_else(|_| "50,100".to_string())
            .split(',')
            .filter_map(|raw| raw.trim().parse::<f64>().ok())
            .filter(|value| *value > 0.0)
            .collect::<Vec<_>>();

        // v0.6 safety & live trading defaults
        let min_liquidity_usd = env::var("MIN_LIQUIDITY_USD")
            .unwrap_or_else(|_| "5000".to_string())
            .parse::<f64>()
            .unwrap_or(5000.0);

        let max_tax_bps = env::var("MAX_TAX_BPS")
            .unwrap_or_else(|_| "500".to_string())
            .parse::<u32>()
            .unwrap_or(500);

        let max_daily_loss_usd = env::var("MAX_DAILY_LOSS_USD")
            .unwrap_or_else(|_| "10.0".to_string())
            .parse::<f64>()
            .unwrap_or(10.0);

        let max_trades_per_hour = env::var("MAX_TRADES_PER_HOUR")
            .unwrap_or_else(|_| "5".to_string())
            .parse::<u32>()
            .unwrap_or(5);

        Ok(Self {
            alchemy_wss,
            alchemy_http,
            dry_run,
            private_key,
            executor_address,
            contract_address,
            telegram_bot_token,
            telegram_chat_id,
            max_gas_price_gwei,
            min_eth_balance,
            max_loss_per_hour_usd,
            max_consecutive_failures,
            min_profit_usd,
            require_simulation,
            enable_live_send,
            uniswap_v3_router,
            aerodrome_router,
            aerodrome_slipstream_router,
            aerodrome_factory,
            execution_gas_limit,
            slippage_bps,
            probe_sizes_usd,
            min_liquidity_usd,
            max_tax_bps,
            max_daily_loss_usd,
            max_trades_per_hour,
        })
    }
}
