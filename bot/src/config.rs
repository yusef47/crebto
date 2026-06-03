use std::env;
use alloy::primitives::Address;
use std::str::FromStr;

#[derive(Debug, Clone)]
pub struct Config {
    pub alchemy_wss: String,
    pub private_key: String,
    pub contract_address: Address,
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

        let alchemy_wss = env::var("ALCHEMY_WSS")
            .map_err(|_| eyre::eyre!("ALCHEMY_WSS must be set in environment"))?;
            
        let private_key = env::var("PRIVATE_KEY")
            .map_err(|_| eyre::eyre!("PRIVATE_KEY must be set in environment"))?;

        let contract_address_str = env::var("CONTRACT_ADDRESS")
            .map_err(|_| eyre::eyre!("CONTRACT_ADDRESS must be set in environment"))?;
        let contract_address = Address::from_str(&contract_address_str)
            .map_err(|_| eyre::eyre!("Invalid CONTRACT_ADDRESS format"))?;

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
            .unwrap_or_else(|_| "5".to_string())
            .parse::<u32>()
            .unwrap_or(5);

        Ok(Self {
            alchemy_wss,
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
