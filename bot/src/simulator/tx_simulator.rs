use alloy::primitives::{Address, Bytes, U256, B256};
use revm::{
    db::{CacheDB, EmptyDB},
    primitives::{AccountInfo, Bytecode, ExecutionResult, TransactTo},
    Evm,
    Database,
};
use eyre::{Result, eyre};

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
}
