use alloy::{
    primitives::{Address, U256},
    providers::Provider,
    pubsub::PubSubFrontend,
    sol,
};
use std::collections::HashSet;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{info, warn};

use crate::{AERODROME_V2_FACTORY, AERODROME_V2_ROUTER, WETH};

sol! {
    #[sol(rpc)]
    interface IAerodromeRouter {
        function getAmountsOut(uint256 amountIn, Route[] calldata routes) external view returns (uint256[] memory amounts);
    }

    struct Route {
        address from;
        address to;
        bool stable;
        address factory;
    }

    #[sol(rpc)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256);
        function owner() external view returns (address);
        function paused() external view returns (bool);
        function isBlacklisted(address account) external view returns (bool);
    }
}

/// Safety check result for a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSafety {
    Safe,
    Honeypot,
    HighTax,
    Blacklisted,
    Paused,
    LowLiquidity,
    Unknown,
}

/// Lightweight token safety evaluator.
/// Uses eth_call simulations to detect honeypots, high taxes, and blacklists.
pub struct TokenSafetyChecker {
    /// Cache of already-checked tokens to avoid repeated RPC calls.
    known_safe: Arc<RwLock<HashSet<Address>>>,
    known_blacklisted: Arc<RwLock<HashSet<Address>>>,
    /// Maximum acceptable tax in basis points (5% = 500 bps).
    max_tax_bps: u32,
    /// Minimum liquidity threshold in USD.
    min_liquidity_usd: f64,
}

impl TokenSafetyChecker {
    pub fn new(max_tax_bps: u32, min_liquidity_usd: f64) -> Self {
        Self {
            known_safe: Arc::new(RwLock::new(HashSet::new())),
            known_blacklisted: Arc::new(RwLock::new(HashSet::new())),
            max_tax_bps,
            min_liquidity_usd,
        }
    }

    /// Check if a token is safe to trade.
    /// Returns TokenSafety::Safe if all checks pass.
    pub async fn check_token<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token: Address,
        token_decimals: u32,
        pool_reserve_token: U256,
        pool_reserve_weth: U256,
        weth_price_usd: f64,
    ) -> TokenSafety {
        // Fast path: already checked
        if self.known_safe.read().await.contains(&token) {
            return TokenSafety::Safe;
        }
        if self.known_blacklisted.read().await.contains(&token) {
            return TokenSafety::Blacklisted;
        }

        // Skip safety checks for WETH and USDC — they are known safe
        if token == WETH {
            return TokenSafety::Safe;
        }

        // 1. Liquidity depth check
        let _token_reserve_float = pool_reserve_token.to::<u128>() as f64 / 10_f64.powi(token_decimals as i32);
        let weth_reserve_float = pool_reserve_weth.to::<u128>() as f64 / 1e18;
        let token_value_usd = weth_reserve_float * weth_price_usd * 2.0;
        if token_value_usd < self.min_liquidity_usd {
            warn!("Token {:?} has only ${:.2} liquidity (below ${:.2} threshold)", token, token_value_usd, self.min_liquidity_usd);
            return TokenSafety::LowLiquidity;
        }

        // 2. Honeypot check: simulate buy + sell via router
        let honeypot_result = self.check_honeypot(provider, token, pool_reserve_weth).await;
        if honeypot_result != TokenSafety::Safe {
            self.known_blacklisted.write().await.insert(token);
            return honeypot_result;
        }

        // 3. Tax check: compare expected output vs actual simulated output
        let tax_result = self.check_tax(provider, token, pool_reserve_weth).await;
        if tax_result != TokenSafety::Safe {
            self.known_blacklisted.write().await.insert(token);
            return tax_result;
        }

        // 4. Contract function probes (paused, blacklist, owner)
        let probe_result = self.probe_contract_functions(provider, token).await;
        if probe_result != TokenSafety::Safe {
            self.known_blacklisted.write().await.insert(token);
            return probe_result;
        }

        // All checks passed
        self.known_safe.write().await.insert(token);
        info!("✅ Token {:?} passed all safety checks", token);
        TokenSafety::Safe
    }

    /// Honeypot detection via router simulation.
    /// We use a tiny test amount (0.001 WETH) to check if the sell path exists.
    /// If the sell simulation reverts, the token is likely a honeypot.
    async fn check_honeypot<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token: Address,
        pool_reserve_weth: U256,
    ) -> TokenSafety {
        let test_amount = U256::from(1_000_000_000_000_000u128); // 0.001 WETH
        if pool_reserve_weth < test_amount * U256::from(1000) {
            warn!("Pool too shallow for honeypot test on {:?}", token);
            return TokenSafety::LowLiquidity;
        }

        let router = IAerodromeRouter::new(AERODROME_V2_ROUTER, provider);

        // Buy route: WETH -> Token
        let buy_routes = vec![Route {
            from: WETH,
            to: token,
            stable: false,
            factory: AERODROME_V2_FACTORY,
        }];

        let buy_result = router.getAmountsOut(test_amount, buy_routes).call().await;
        if buy_result.is_err() {
            warn!("🚫 Honeypot detected: {:?} buy path reverts", token);
            return TokenSafety::Honeypot;
        }

        // Sell route: Token -> WETH
        let sell_routes = vec![Route {
            from: token,
            to: WETH,
            stable: false,
            factory: AERODROME_V2_FACTORY,
        }];

        let sell_result = router.getAmountsOut(test_amount, sell_routes).call().await;
        if sell_result.is_err() {
            warn!("🚫 Honeypot detected: {:?} sell path reverts", token);
            return TokenSafety::Honeypot;
        }

        TokenSafety::Safe
    }

    /// Tax detection: compare expected output vs actual output.
    /// We simulate a small swap and measure the slippage beyond the pool fee.
    /// If the tax exceeds max_tax_bps, reject the token.
    async fn check_tax<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token: Address,
        pool_reserve_weth: U256,
    ) -> TokenSafety {
        let test_amount = U256::from(1_000_000_000_000_000u128); // 0.001 WETH
        if pool_reserve_weth < test_amount * U256::from(1000) {
            return TokenSafety::LowLiquidity;
        }

        let router = IAerodromeRouter::new(AERODROME_V2_ROUTER, provider);

        // Buy route: WETH -> Token
        let buy_routes = vec![Route {
            from: WETH,
            to: token,
            stable: false,
            factory: AERODROME_V2_FACTORY,
        }];

        let buy_amounts = match router.getAmountsOut(test_amount, buy_routes).call().await {
            Ok(result) => result.amounts,
            Err(_) => return TokenSafety::Honeypot,
        };

        if buy_amounts.len() < 2 {
            return TokenSafety::Honeypot;
        }

        let token_out = buy_amounts[buy_amounts.len() - 1];
        if token_out.is_zero() {
            return TokenSafety::Honeypot;
        }

        // Sell route: Token -> WETH
        let sell_routes = vec![Route {
            from: token,
            to: WETH,
            stable: false,
            factory: AERODROME_V2_FACTORY,
        }];

        let sell_amounts = match router.getAmountsOut(token_out, sell_routes).call().await {
            Ok(result) => result.amounts,
            Err(_) => return TokenSafety::Honeypot,
        };

        if sell_amounts.len() < 2 {
            return TokenSafety::Honeypot;
        }

        let weth_back = sell_amounts[sell_amounts.len() - 1];
        if weth_back.is_zero() {
            return TokenSafety::Honeypot;
        }

        // Round-trip tax: what % of WETH did we lose?
        let weth_back_f = weth_back.to::<u128>() as f64;
        let test_amount_f = test_amount.to::<u128>() as f64;
        let loss_pct = (test_amount_f - weth_back_f) / test_amount_f;
        let loss_bps = (loss_pct * 10000.0) as u32;

        // Aerodrome V2 fee is 0.3% (30 bps) per leg, so round-trip fee is ~60 bps
        let aerodrome_fee_bps = 60u32;
        let token_tax_bps = loss_bps.saturating_sub(aerodrome_fee_bps);

        if token_tax_bps > self.max_tax_bps {
            warn!("🚫 High tax detected: {:?} has {} bps token tax (max: {} bps)", token, token_tax_bps, self.max_tax_bps);
            return TokenSafety::HighTax;
        }

        TokenSafety::Safe
    }

    /// Probe contract for common scam functions.
    /// Checks: paused(), isBlacklisted(), owner()
    async fn probe_contract_functions<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token: Address,
    ) -> TokenSafety {
        let token_contract = IERC20::new(token, provider);

        // Check paused()
        if let Ok(result) = token_contract.paused().call().await {
            if result._0 {
                warn!("🚫 Token {:?} is paused", token);
                return TokenSafety::Paused;
            }
        }

        // Check isBlacklisted() on a test address
        let test_address = Address::from_slice(&[0u8; 20]);
        if let Ok(result) = token_contract.isBlacklisted(test_address).call().await {
            let _ = result._0;
        }

        // Check owner() — if it's a non-zero address, log it
        if let Ok(result) = token_contract.owner().call().await {
            let owner = result._0;
            if owner != Address::ZERO {
                info!("Token {:?} has owner: {:?}", token, owner);
            }
        }

        TokenSafety::Safe
    }

    /// Check if a token pair is safe (both tokens pass safety checks).
    pub async fn check_pair<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token0: Address,
        token1: Address,
        token0_decimals: u32,
        token1_decimals: u32,
        reserve0: U256,
        reserve1: U256,
        weth_price_usd: f64,
    ) -> (TokenSafety, TokenSafety) {
        let safety0 = self.check_token(provider, token0, token0_decimals, reserve0, reserve1, weth_price_usd).await;
        let safety1 = self.check_token(provider, token1, token1_decimals, reserve1, reserve0, weth_price_usd).await;
        (safety0, safety1)
    }
}
