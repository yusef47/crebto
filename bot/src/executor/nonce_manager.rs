use std::sync::atomic::{AtomicU64, Ordering};
use alloy::{
    network::Ethereum,
    providers::Provider,
    transports::Transport,
};
use alloy::primitives::Address;
use tracing::info;

pub struct NonceManager {
    nonce: AtomicU64,
}

impl NonceManager {
    pub fn new(initial_nonce: u64) -> Self {
        Self {
            nonce: AtomicU64::new(initial_nonce),
        }
    }

    pub async fn initialize<T, P>(
        provider: &P,
        address: Address,
    ) -> Result<Self, eyre::Report>
    where
        T: Transport + Clone,
        P: Provider<T, Ethereum>,
    {
        let chain_nonce = provider.get_transaction_count(address).await?;
        info!("Initialized Nonce Manager with chain nonce: {}", chain_nonce);
        Ok(Self::new(chain_nonce))
    }

    /// Returns the current nonce and increments the local counter
    pub fn next_nonce(&self) -> u64 {
        self.nonce.fetch_add(1, Ordering::SeqCst)
    }

    /// Resets the local nonce counter to a specific value (e.g. after transaction reverts/mempool cleared)
    pub fn reset(&self, new_nonce: u64) {
        self.nonce.store(new_nonce, Ordering::SeqCst);
        info!("Reset local nonce to: {}", new_nonce);
    }
}
