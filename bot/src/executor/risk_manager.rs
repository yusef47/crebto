use alloy::primitives::U256;
use std::time::{Duration, Instant};

#[derive(Debug, Clone)]
pub struct RiskLimits {
    pub max_gas_price_wei: U256,
    pub min_balance_wei: U256,
    pub max_loss_per_hour_wei: U256,
    pub min_net_profit_wei: U256,
    pub require_simulation: bool,
}

#[derive(Debug, Clone)]
pub struct ExecutionRisk {
    pub wallet_balance_wei: U256,
    pub gas_price_wei: U256,
    pub estimated_gas_units: u64,
    pub net_profit_wei: U256,
    pub simulation_success: bool,
}

pub struct RiskManager {
    limits: RiskLimits,
    window_started_at: Instant,
    loss_this_window_wei: U256,
}

impl RiskManager {
    pub fn new(limits: RiskLimits) -> Self {
        Self {
            limits,
            window_started_at: Instant::now(),
            loss_this_window_wei: U256::ZERO,
        }
    }

    pub fn approve(&mut self, risk: &ExecutionRisk) -> Result<(), String> {
        self.reset_window_if_needed();

        if self.limits.require_simulation && !risk.simulation_success {
            return Err("simulation failed or was not run".to_string());
        }

        if risk.gas_price_wei > self.limits.max_gas_price_wei {
            return Err(format!(
                "gas price {} exceeds cap {}",
                risk.gas_price_wei, self.limits.max_gas_price_wei
            ));
        }

        if risk.wallet_balance_wei < self.limits.min_balance_wei {
            return Err(format!(
                "wallet balance {} is below minimum {}",
                risk.wallet_balance_wei, self.limits.min_balance_wei
            ));
        }

        let max_gas_cost = risk.gas_price_wei * U256::from(risk.estimated_gas_units);
        if risk.wallet_balance_wei < max_gas_cost + self.limits.min_balance_wei {
            return Err("wallet balance cannot cover gas while preserving minimum balance".to_string());
        }

        if risk.net_profit_wei < self.limits.min_net_profit_wei {
            return Err(format!(
                "net profit {} is below minimum {}",
                risk.net_profit_wei, self.limits.min_net_profit_wei
            ));
        }

        if self.loss_this_window_wei + max_gas_cost > self.limits.max_loss_per_hour_wei {
            return Err("hourly gas-loss budget would be exceeded".to_string());
        }

        Ok(())
    }

    pub fn record_failed_tx_gas(&mut self, gas_cost_wei: U256) {
        self.reset_window_if_needed();
        self.loss_this_window_wei += gas_cost_wei;
    }

    fn reset_window_if_needed(&mut self) {
        if self.window_started_at.elapsed() >= Duration::from_secs(3600) {
            self.window_started_at = Instant::now();
            self.loss_this_window_wei = U256::ZERO;
        }
    }
}
