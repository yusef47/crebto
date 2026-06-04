use alloy::primitives::{Address, Bytes, U256};
use revm::{
    db::{CacheDB, EmptyDB},
    primitives::{AccountInfo, Bytecode, ExecutionResult, TransactTo},
    Evm,
    Database,
};
use eyre::{Result, eyre};
use futures_util::{stream::FuturesUnordered, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Debug, Clone)]
pub struct SimulationRequest {
    pub caller: Address,
    pub contract_address: Address,
    pub call_data: Bytes,
    pub gas_limit: u64,
    pub gas_price: U256,
}

#[derive(Debug, Clone)]
pub struct SimulationOutcome {
    pub success: bool,
    pub gas_used: u64,
    pub gas_cost_wei: U256,
    pub return_data: Bytes,
    pub revert_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct SizedSimulationRequest {
    pub amount_in: U256,
    pub expected_profit_wei: U256,
    pub native_token_units_in_profit_asset: U256,
    pub request: SimulationRequest,
}

#[derive(Debug, Clone)]
pub struct SizedSimulationOutcome {
    pub amount_in: U256,
    pub expected_profit_wei: U256,
    pub net_profit_wei: U256,
    pub request: SimulationRequest,
    pub simulation: SimulationOutcome,
}

#[derive(Debug, Clone)]
pub struct SizeOptimizationResult {
    pub best: Option<SizedSimulationOutcome>,
    pub attempts: Vec<SizedSimulationOutcome>,
    pub rejected: Vec<SizedSimulationOutcome>,
}

#[derive(Debug, Deserialize)]
struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

#[derive(Debug, Deserialize)]
struct RpcResponse {
    result: Option<Value>,
    error: Option<RpcError>,
}

pub struct TxSimulator {
    db: CacheDB<EmptyDB>,
    rpc_url: Option<String>,
    http: Client,
}

impl TxSimulator {
    pub fn new() -> Self {
        // CacheDB allows us to mock the blockchain state locally
        let db = CacheDB::new(EmptyDB::default());
        Self {
            db,
            rpc_url: None,
            http: Client::new(),
        }
    }

    pub fn provider_backed(rpc_url: String) -> Self {
        let db = CacheDB::new(EmptyDB::default());
        Self {
            db,
            rpc_url: Some(rpc_url),
            http: Client::new(),
        }
    }

    /// Deploys the mock bytecode for contract at a specific address in our CacheDB
    pub fn deploy_mock_contract(&mut self, address: Address, code: Bytes) {
        let revm_address = revm::primitives::Address::from_slice(address.as_ref());
        let revm_code = revm::primitives::Bytes::copy_from_slice(code.as_ref());
        let bytecode = Bytecode::new_raw(revm_code);
        let info = AccountInfo {
            balance: revm::primitives::U256::ZERO,
            nonce: 0,
            code_hash: revm::primitives::B256::from_slice(&[0u8; 32]), // Placeholder
            code: Some(bytecode),
        };
        self.db.insert_account_info(revm_address, info);
    }

    /// Sets the token balance of an address in our CacheDB
    pub fn set_balance(&mut self, address: Address, balance: U256) {
        let revm_address = revm::primitives::Address::from_slice(address.as_ref());
        let mut info = self.db.basic(revm_address).unwrap_or(None).unwrap_or_default();
        let balance_bytes = balance.to_be_bytes::<32>();
        info.balance = revm::primitives::U256::from_be_bytes(balance_bytes);
        self.db.insert_account_info(revm_address, info);
    }

    /// Runs a simulation of the arbitrage transaction in revm
    pub async fn simulate_tx(
        &mut self,
        caller: Address,
        contract_address: Address,
        call_data: Bytes,
        gas_limit: u64,
        gas_price: U256,
    ) -> Result<(bool, u64, U256)> {
        if self.rpc_url.is_some() {
            let request = SimulationRequest {
                caller,
                contract_address,
                call_data,
                gas_limit,
                gas_price,
            };
            let outcome = self.simulate_provider_request(&request).await?;
            return Ok((
                outcome.success,
                outcome.gas_used,
                u256_from_return_data(&outcome.return_data),
            ));
        }

        let revm_caller = revm::primitives::Address::from_slice(caller.as_ref());
        let revm_contract = revm::primitives::Address::from_slice(contract_address.as_ref());
        let revm_calldata = revm::primitives::Bytes::copy_from_slice(call_data.as_ref());
        let gas_price_bytes = gas_price.to_be_bytes::<32>();
        let revm_gas_price = revm::primitives::U256::from_be_bytes(gas_price_bytes);

        // Setup the transaction environment using revm 9.0 builder
        let mut evm = Evm::builder()
            .with_db(&mut self.db)
            .modify_tx_env(|tx| {
                tx.caller = revm_caller;
                tx.transact_to = TransactTo::Call(revm_contract);
                tx.data = revm_calldata;
                tx.gas_limit = gas_limit;
                tx.gas_price = revm_gas_price;
                tx.value = revm::primitives::U256::ZERO;
            })
            .build();

        // Run transaction
        let ref_tx = evm.transact()?;
        let result = ref_tx.result;

        match result {
            ExecutionResult::Success { gas_used, output, .. } => {
                // If success, return success status, gas used and return value
                let return_value = match output {
                    revm::primitives::Output::Call(bytes) => {
                        U256::from_be_slice(bytes.as_ref())
                    }
                    _ => U256::ZERO,
                };
                Ok((true, gas_used, return_value))
            }
            ExecutionResult::Revert { gas_used, .. } => {
                // Transaction reverted
                Ok((false, gas_used, U256::ZERO))
            }
            ExecutionResult::Halt { reason, gas_used } => {
                Err(eyre!("Simulation halted: {:?}", reason))
            }
        }
    }

    pub async fn simulate_provider_request(
        &self,
        request: &SimulationRequest,
    ) -> Result<SimulationOutcome> {
        let call_tx = json!({
            "from": address_to_rpc_hex(request.caller),
            "to": address_to_rpc_hex(request.contract_address),
            "data": bytes_to_rpc_hex(&request.call_data),
            "gas": u64_to_rpc_hex(request.gas_limit),
            "gasPrice": u256_to_rpc_hex(request.gas_price),
            "value": "0x0",
        });

        let call_result = self
            .rpc("eth_call", json!([call_tx.clone(), "pending"]))
            .await;

        let return_data = match call_result {
            Ok(value) => {
                let hex = value
                    .as_str()
                    .ok_or_else(|| eyre!("eth_call returned non-string result: {}", value))?;
                Bytes::from(hex_to_bytes(hex)?)
            }
            Err(e) => {
                return Ok(SimulationOutcome {
                    success: false,
                    gas_used: request.gas_limit,
                    gas_cost_wei: U256::from(request.gas_limit) * request.gas_price,
                    return_data: Bytes::new(),
                    revert_reason: Some(e.to_string()),
                });
            }
        };

        let gas_used = match self.rpc("eth_estimateGas", json!([call_tx])).await {
            Ok(value) => value
                .as_str()
                .and_then(|hex| u64::from_str_radix(hex.trim_start_matches("0x"), 16).ok())
                .unwrap_or(request.gas_limit),
            Err(_) => request.gas_limit,
        };

        Ok(SimulationOutcome {
            success: true,
            gas_used,
            gas_cost_wei: U256::from(gas_used) * request.gas_price,
            return_data,
            revert_reason: None,
        })
    }

    pub async fn optimize_size_grid(
        &self,
        candidates: Vec<SizedSimulationRequest>,
    ) -> Result<SizeOptimizationResult> {
        if self.rpc_url.is_none() {
            return Err(eyre!("Provider-backed simulator is required for size optimization"));
        }

        let mut futures = FuturesUnordered::new();
        for candidate in candidates {
            futures.push(async move {
                let simulation = self.simulate_provider_request(&candidate.request).await?;
                let gas_cost_in_profit_asset = if candidate.native_token_units_in_profit_asset.is_zero() {
                    simulation.gas_cost_wei
                } else {
                    (simulation.gas_cost_wei * candidate.native_token_units_in_profit_asset)
                        / U256::from(1_000_000_000_000_000_000u128)
                };
                let net_profit_wei = if simulation.success
                    && candidate.expected_profit_wei > gas_cost_in_profit_asset
                {
                    candidate.expected_profit_wei - gas_cost_in_profit_asset
                } else {
                    U256::ZERO
                };

                Ok::<SizedSimulationOutcome, eyre::Report>(SizedSimulationOutcome {
                    amount_in: candidate.amount_in,
                    expected_profit_wei: candidate.expected_profit_wei,
                    net_profit_wei,
                    request: candidate.request,
                    simulation,
                })
            });
        }

        let mut attempts = Vec::new();
        let mut rejected = Vec::new();
        let mut best: Option<SizedSimulationOutcome> = None;

        while let Some(result) = futures.next().await {
            let outcome = result?;
            if outcome.simulation.success && outcome.net_profit_wei > U256::ZERO {
                if best
                    .as_ref()
                    .map(|current| outcome.net_profit_wei > current.net_profit_wei)
                    .unwrap_or(true)
                {
                    best = Some(outcome.clone());
                }
                attempts.push(outcome);
            } else {
                rejected.push(outcome);
            }
        }

        Ok(SizeOptimizationResult {
            best,
            attempts,
            rejected,
        })
    }

    async fn rpc(&self, method: &str, params: Value) -> Result<Value> {
        let rpc_url = self
            .rpc_url
            .as_ref()
            .ok_or_else(|| eyre!("TxSimulator was not configured with an RPC URL"))?;

        let response = self
            .http
            .post(rpc_url)
            .json(&json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": method,
                "params": params,
            }))
            .send()
            .await?
            .error_for_status()?
            .json::<RpcResponse>()
            .await?;

        if let Some(error) = response.error {
            let reason = error
                .data
                .as_ref()
                .and_then(extract_revert_data)
                .and_then(|data| decode_revert_reason(&data))
                .unwrap_or_else(|| error.message.clone());
            return Err(eyre!("RPC {} failed ({}): {}", method, error.code, reason));
        }

        response
            .result
            .ok_or_else(|| eyre!("RPC {} returned neither result nor error", method))
    }
}

fn address_to_rpc_hex(address: Address) -> String {
    format!("0x{}", bytes_to_hex(address.as_ref()))
}

fn bytes_to_rpc_hex(bytes: &Bytes) -> String {
    format!("0x{}", bytes_to_hex(bytes.as_ref()))
}

fn u64_to_rpc_hex(value: u64) -> String {
    format!("0x{:x}", value)
}

fn u256_to_rpc_hex(value: U256) -> String {
    if value.is_zero() {
        return "0x0".to_string();
    }
    let bytes = value.to_be_bytes::<32>();
    let first_non_zero = bytes.iter().position(|byte| *byte != 0).unwrap_or(0);
    format!("0x{}", bytes_to_hex(&bytes[first_non_zero..]))
}

fn bytes_to_hex(bytes: &[u8]) -> String {
    const LUT: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(LUT[(byte >> 4) as usize] as char);
        out.push(LUT[(byte & 0x0f) as usize] as char);
    }
    out
}

fn hex_to_bytes(hex: &str) -> Result<Vec<u8>> {
    let hex = hex.trim_start_matches("0x");
    if hex.is_empty() {
        return Ok(Vec::new());
    }
    if hex.len() % 2 != 0 {
        return Err(eyre!("Odd-length hex string"));
    }

    let mut bytes = Vec::with_capacity(hex.len() / 2);
    let chars: Vec<char> = hex.chars().collect();
    for i in (0..chars.len()).step_by(2) {
        let high = chars[i]
            .to_digit(16)
            .ok_or_else(|| eyre!("Invalid hex character"))?;
        let low = chars[i + 1]
            .to_digit(16)
            .ok_or_else(|| eyre!("Invalid hex character"))?;
        bytes.push(((high << 4) | low) as u8);
    }
    Ok(bytes)
}

fn extract_revert_data(value: &Value) -> Option<String> {
    if let Some(data) = value.as_str() {
        return Some(data.to_string());
    }
    if let Some(data) = value.get("data").and_then(Value::as_str) {
        return Some(data.to_string());
    }
    if let Some(data) = value.get("result").and_then(Value::as_str) {
        return Some(data.to_string());
    }
    None
}

fn decode_revert_reason(data_hex: &str) -> Option<String> {
    let bytes = hex_to_bytes(data_hex).ok()?;
    if bytes.len() >= 4 && bytes[0..4] == [0x08, 0xc3, 0x79, 0xa0] {
        if bytes.len() < 68 {
            return None;
        }
        let len = U256::from_be_slice(&bytes[36..68]).to::<usize>();
        let start = 68;
        let end = start + len;
        if bytes.len() < end {
            return None;
        }
        return String::from_utf8(bytes[start..end].to_vec()).ok();
    }

    if bytes.len() >= 4 && bytes[0..4] == [0x4e, 0x48, 0x7b, 0x71] {
        return Some("Solidity panic".to_string());
    }

    None
}

fn u256_from_return_data(return_data: &Bytes) -> U256 {
    let data = return_data.as_ref();
    if data.len() >= 32 {
        U256::from_be_slice(&data[0..32])
    } else {
        U256::ZERO
    }
}
