use alloy::{
    network::TransactionBuilder,
    primitives::{Address, Bytes, U256},
    providers::Provider,
    pubsub::PubSubFrontend,
    rpc::types::eth::TransactionRequest,
    signers::local::PrivateKeySigner,
    sol,
};
use crate::executor::nonce_manager::NonceManager;
use eyre::Result;
use std::sync::Arc;
use tracing::info;

// Auto-generate ABI encoders from Solidity definitions
sol! {
    struct SwapStep {
        address target;
        bytes callData;
    }

    interface IFlashArb {
        function executeArbitrage(
            address asset,
            uint256 amount,
            SwapStep[] calldata swapSteps
        ) external;
    }
}

pub struct TxBuilder {
    contract_address: Address,
    signer: PrivateKeySigner,
}

impl TxBuilder {
    pub fn new(contract_address: Address, private_key: &str) -> Result<Self> {
        let signer = private_key.parse::<PrivateKeySigner>()?;
        Ok(Self {
            contract_address,
            signer,
        })
    }

    /// Prepares call data for the FlashArb executeArbitrage call
    pub fn encode_arbitrage_call(
        &self,
        asset: Address,
        amount: U256,
        steps: Vec<(Address, Bytes)>,
    ) -> Bytes {
        let swap_steps: Vec<SwapStep> = steps
            .into_iter()
            .map(|(target, call_data)| SwapStep {
                target,
                callData: call_data,
            })
            .collect();

        let call = IFlashArb::executeArbitrageCall {
            asset,
            amount,
            swapSteps: swap_steps,
        };

        Bytes::from(call.abi_encode())
    }

    /// Sends the signed transaction to Base
    pub async fn send_transaction<P: Provider<T>, T: PubSubFrontend>(
        &self,
        provider: &Arc<P>,
        nonce_manager: &NonceManager,
        call_data: Bytes,
        gas_limit: u64,
        max_fee_per_gas: U256,
        max_priority_fee_per_gas: U256,
    ) -> Result<alloy::primitives::TxHash> {
        let nonce = nonce_manager.next_nonce();

        let tx = TransactionRequest::default()
            .to(self.contract_address)
            .input(call_data.into())
            .nonce(nonce)
            .gas_limit(gas_limit)
            .max_fee_per_gas(max_fee_per_gas.to::<u128>())
            .max_priority_fee_per_gas(max_priority_fee_per_gas.to::<u128>());

        info!("Sending arbitrage TX with nonce: {}, gas limit: {}", nonce, gas_limit);

        // Sign and send the transaction
        let tx_envelope = tx.build(&self.signer).await?;
        let receipt = provider.send_tx_envelope(tx_envelope).await?;

        Ok(*receipt.tx_hash())
    }
}
