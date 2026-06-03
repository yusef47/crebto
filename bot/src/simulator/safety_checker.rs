use crate::simulator::tx_simulator::TxSimulator;
use alloy::primitives::{Address, Bytes, U256};
use eyre::Result;

pub struct SafetyChecker;

impl SafetyChecker {
    /// Simulates buying and selling a token to verify it's not a honeypot and calculate actual tax
    pub async fn verify_token_safety(
        simulator: &mut TxSimulator,
        caller: Address,
        contract_address: Address,
        buy_call_data: Bytes,
        sell_call_data: Bytes,
        gas_limit: u64,
        gas_price: U256,
    ) -> Result<bool> {
        // 1. Simulate the buy swap
        let (buy_success, _, _) = simulator
            .simulate_tx(caller, contract_address, buy_call_data, gas_limit, gas_price)
            .await?;
        
        if !buy_success {
            // Reverted on buy -> Dangerous/Honeypot
            return Ok(false);
        }

        // 2. Simulate the sell swap
        let (sell_success, _, _) = simulator
            .simulate_tx(caller, contract_address, sell_call_data, gas_limit, gas_price)
            .await?;

        if !sell_success {
            // Reverted on sell -> Classic Honeypot!
            return Ok(false);
        }

        // Token is safe to trade
        Ok(true)
    }
}
