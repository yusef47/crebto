use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use tokio::fs;

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BotCheckpoint {
    pub last_processed_block: u64,
    pub watchlist: Vec<String>,
    pub metrics: HashMap<String, String>,
    pub total_wins: u64,
    pub total_profit_usd: f64,
    pub last_run_timestamp: u64,
}

impl BotCheckpoint {
    pub async fn save(&self, path: &str) -> eyre::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json).await?;
        Ok(())
    }

    pub async fn load(path: &str) -> eyre::Result<Self> {
        match fs::read_to_string(path).await {
            Ok(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            Err(_) => Ok(Self::default()),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct BorrowerWatchlist {
    pub borrowers: Vec<BorrowerEntry>,
    pub updated_at: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BorrowerEntry {
    pub address: String,
    pub total_debt_usd: u64,
    pub total_collateral_usd: u64,
    pub health_factor: String,
    pub collateral_assets: Vec<String>,
    pub debt_assets: Vec<String>,
}

impl BorrowerWatchlist {
    pub async fn save(&self, path: &str) -> eyre::Result<()> {
        let json = serde_json::to_string_pretty(self)?;
        fs::write(path, json).await?;
        Ok(())
    }

    pub async fn load(path: &str) -> eyre::Result<Self> {
        match fs::read_to_string(path).await {
            Ok(json) => Ok(serde_json::from_str(&json).unwrap_or_default()),
            Err(_) => Ok(Self::default()),
        }
    }
}
