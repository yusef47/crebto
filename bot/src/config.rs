use std::env;
use alloy::primitives::Address;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Config {
    pub alchemy_wss: String,
    pub alchemy_http: String,
    pub dry_run: bool,
    pub private_key: Option<String>,
    pub contract_address: Option<Address>,
    pub telegram_bot_token: Option<String>,
    pub telegram_chat_id: Option<String>,
    pub max_gas_price_gwei: u64,
    pub min_eth_balance: f64,
    pub max_loss_per_hour_usd: f64,
    pub max_consecutive_failures: u32,
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

        Ok(Self {
            alchemy_wss,
            alchemy_http,
            dry_run,
            private_key,
            contract_address,
            telegram_bot_token,
            telegram_chat_id,
            max_gas_price_gwei,
            min_eth_balance,
            max_loss_per_hour_usd,
            max_consecutive_failures,
        })
    }
}
