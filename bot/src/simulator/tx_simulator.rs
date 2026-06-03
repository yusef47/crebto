use alloy::primitives::{Address, Bytes, U256, B256};
use revm::{
    db::{CacheDB, EmptyDB},
    primitives::{AccountInfo, Bytecode, ExecutionResult, TransactTo, TxEnv},
    EVM,
};
use eyre::{Result, eyre};
use std::str::FromStr;

pub struct TxSimulator {
    db: CacheDB<EmptyDB>,
}

impl TxSimulator {
    pub fn new() -> Self {
        // CacheDB allows us to mock the blockchain state locally
        let db = CacheDB::new(EmptyDB::default());
        Self { db }
    }

    /// Deploys the mock bytecode for contract at a specific address in our CacheDB
    pub fn deploy_mock_contract(&mut self, address: Address, code: Bytes) {
        let bytecode = Bytecode::new_raw(code.into());
        let info = AccountInfo {
            balance: U256::ZERO,
            nonce: 0,
            code_hash: B256::from_slice(&[0u8; 32]), // Placeholder
            code: Some(bytecode),
        };
        self.db.insert_account_info(address, info);
    }

    /// Sets the token balance of an address in our CacheDB
    pub fn set_balance(&mut self, address: Address, balance: U256) {
        let mut info = self.db.basic(address).unwrap_or(None).unwrap_or_default();
        info.balance = balance;
        self.db.insert_account_info(address, info);
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
        let mut evm = EVM::new();
        evm.database(&mut self.db);

        // Setup the transaction environment
        let mut tx = TxEnv::default();
        tx.caller = caller;
        tx.transact_to = TransactTo::Call(contract_address);
        tx.data = call_data.into();
        tx.gas_limit = gas_limit;
        tx.gas_price = gas_price;
        tx.value = U256::ZERO;

        evm.env.tx = tx;

        // Run transaction
        let ref_tx = evm.transact()?;
        let result = ref_tx.result;

        match result {
            ExecutionResult::Success { gas_used, output, .. } => {
                // If success, return success status, gas used and return value
                let return_value = match output {
                    revm::primitives::Output::Call(bytes) => U256::from_be_slice(&bytes),
                    _ => U256::ZERO,
                };
                Ok((true, gas_used, return_value))
            }
            ExecutionResult::Revert { gas_used, output } => {
                // Transaction reverted
                Ok((false, gas_used, U256::ZERO))
            }
            ExecutionResult::Halt { reason, gas_used } => {
                Err(eyre!("Simulation halted: {:?}", reason))
            }
        }
    }
}
