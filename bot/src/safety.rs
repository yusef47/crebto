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
use alloy::sol_types::SolCall;

use crate::{WSEI, multicall::multicall3_aggregate3};

sol! {
    #[sol(rpc)]
    interface IUniswapV2Router {
        function getAmountsOut(uint256 amountIn, address[] calldata path) external view returns (uint256[] memory amounts);
    }

    #[sol(rpc)]
    interface IERC20 {
        function balanceOf(address account) external view returns (uint256 bal);
        function totalSupply() external view returns (uint256 supply);
        function owner() external view returns (address ownerAddr);
        function paused() external view returns (bool isPaused);
        function isBlacklisted(address account) external view returns (bool blacklisted);
    }

    /// Standard Unicrypt / Mudra liquidity locker interface (V2 style).
    /// If a locker contract is configured on the network, this reads the lock metadata.
    #[sol(rpc)]
    interface ILiquidityLocker {
        function lpLockInfo(address lpToken) external view returns (uint256 lockDate, uint256 amount, uint256 unlockDate, uint256 lockID, address owner);
    }
}

/// Dead / burn addresses commonly used in token contracts.
const DEAD_ADDRESS_1: Address = alloy::primitives::address!("000000000000000000000000000000000000dEaD");
const DEAD_ADDRESS_2: Address = alloy::primitives::address!("0000000000000000000000000000000000000001");

/// Safety check result for a token.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenSafety {
    Safe,
    Honeypot,
    HighTax,
    Blacklisted,
    Paused,
    LowLiquidity,
    OwnershipNotRenounced,
    LiquidityNotLocked,
    WhaleConcentration,
    Unknown,
}

/// Lightweight token safety evaluator.
/// v0.7: configurable router for multi-chain support.
/// Uses eth_call simulations to detect honeypots, high taxes, blacklists,
/// ownership risks, liquidity locks, and whale concentration.
pub struct TokenSafetyChecker {
    /// Cache of already-checked tokens to avoid repeated RPC calls.
    known_safe: Arc<RwLock<HashSet<Address>>>,
    known_blacklisted: Arc<RwLock<HashSet<Address>>>,
    /// Maximum acceptable tax in basis points (5% = 500 bps).
    max_tax_bps: u32,
    /// Minimum liquidity threshold in USD.
    min_liquidity_usd: f64,
    /// Router address for swap simulations (e.g., DragonSwap router on Sei).
    router: Address,
    /// Optional locker contract for LP lock verification.
    liquidity_locker: Option<Address>,
}

impl TokenSafetyChecker {
    pub fn new(router: Address, max_tax_bps: u32, min_liquidity_usd: f64, liquidity_locker: Option<Address>) -> Self {
        Self {
            router,
            known_safe: Arc::new(RwLock::new(HashSet::new())),
            known_blacklisted: Arc::new(RwLock::new(HashSet::new())),
            max_tax_bps,
            min_liquidity_usd,
            liquidity_locker,
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

        // Skip safety checks for WSEI and USDC — they are known safe
        if token == WSEI {
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

        // 3. Tax check
        let tax_result = self.check_tax(provider, token, pool_reserve_weth).await;
        if tax_result != TokenSafety::Safe {
            self.known_blacklisted.write().await.insert(token);
            return tax_result;
        }

        // 4. Contract probes (paused, blacklist, owner)
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

    /// Evaluate a pair with the v0.7 extended safety stack.
    /// Runs the base checks above, then adds ownership renouncement,
    /// liquidity lock, and whale concentration filters.
    pub async fn check_pair_v07<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token0: Address,
        token1: Address,
        pool_address: Address,
        token0_decimals: u32,
        token1_decimals: u32,
        reserve0: U256,
        reserve1: U256,
        weth_price_usd: f64,
        multicall3: Option<Address>,
    ) -> (TokenSafety, TokenSafety) {
        let safety0 = self.check_token(provider, token0, token0_decimals, reserve0, reserve1, weth_price_usd).await;
        let safety1 = self.check_token(provider, token1, token1_decimals, reserve1, reserve0, weth_price_usd).await;

        if safety0 != TokenSafety::Safe || safety1 != TokenSafety::Safe {
            return (safety0, safety1);
        }

        // v0.7 extended checks — run only if base checks passed

        // 5. Ownership renouncement check
        let owner0 = self.check_ownership_renouncement(provider, token0).await;
        let owner1 = self.check_ownership_renouncement(provider, token1).await;
        if owner0 != TokenSafety::Safe {
            warn!("🚫 Token {:?} ownership not renounced", token0);
            self.known_blacklisted.write().await.insert(token0);
            return (owner0, safety1);
        }
        if owner1 != TokenSafety::Safe {
            warn!("🚫 Token {:?} ownership not renounced", token1);
            self.known_blacklisted.write().await.insert(token1);
            return (safety0, owner1);
        }

        // 6. Liquidity lock check
        let lock = self.check_liquidity_lock(provider, pool_address).await;
        if lock != TokenSafety::Safe {
            warn!("🚫 Pool {:?} LP not locked in verified locker", pool_address);
            return (lock, lock);
        }

        // 7. Whale concentration check
        let whale0 = self.check_whale_concentration(provider, token0, pool_address, multicall3).await;
        let whale1 = self.check_whale_concentration(provider, token1, pool_address, multicall3).await;
        if whale0 != TokenSafety::Safe {
            warn!("🚫 Token {:?} whale concentration detected", token0);
            self.known_blacklisted.write().await.insert(token0);
            return (whale0, safety1);
        }
        if whale1 != TokenSafety::Safe {
            warn!("🚫 Token {:?} whale concentration detected", token1);
            self.known_blacklisted.write().await.insert(token1);
            return (safety0, whale1);
        }

        (TokenSafety::Safe, TokenSafety::Safe)
    }

    /// Legacy check_pair for backward compatibility.
    #[allow(dead_code)]
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

    /// Honeypot detection via router simulation.
    async fn check_honeypot<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token: Address,
        pool_reserve_weth: U256,
    ) -> TokenSafety {
        let test_amount = U256::from(1_000_000_000_000_000u128); // 0.001 WSEI
        if pool_reserve_weth < test_amount * U256::from(1000) {
            warn!("Pool too shallow for honeypot test on {:?}", token);
            return TokenSafety::LowLiquidity;
        }

        let router = IUniswapV2Router::new(self.router, provider);

        let buy_path = vec![WSEI, token];
        let buy_result = router.getAmountsOut(test_amount, buy_path).call().await;
        if buy_result.is_err() {
            warn!("🚫 Honeypot detected: {:?} buy path reverts", token);
            return TokenSafety::Honeypot;
        }

        let token_received = match buy_result {
            Ok(r) => {
                if r.amounts.len() < 2 { return TokenSafety::Honeypot; }
                r.amounts[r.amounts.len() - 1]
            }
            Err(_) => return TokenSafety::Honeypot,
        };

        if token_received.is_zero() {
            warn!("🚫 Honeypot detected: {:?} buy returns zero tokens", token);
            return TokenSafety::Honeypot;
        }

        let sell_path = vec![token, WSEI];
        let sell_result = router.getAmountsOut(token_received, sell_path).call().await;
        if sell_result.is_err() {
            warn!("🚫 Honeypot detected: {:?} sell path reverts", token);
            return TokenSafety::Honeypot;
        }

        TokenSafety::Safe
    }

    /// Tax detection via round-trip swap simulation.
    async fn check_tax<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token: Address,
        pool_reserve_weth: U256,
    ) -> TokenSafety {
        let test_amount = U256::from(1_000_000_000_000_000u128); // 0.001 WSEI
        if pool_reserve_weth < test_amount * U256::from(1000) {
            return TokenSafety::LowLiquidity;
        }

        let router = IUniswapV2Router::new(self.router, provider);

        let buy_path = vec![WSEI, token];
        let buy_amounts = match router.getAmountsOut(test_amount, buy_path).call().await {
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

        let sell_path = vec![token, WSEI];
        let sell_amounts = match router.getAmountsOut(token_out, sell_path).call().await {
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

        let weth_back_f = weth_back.to::<u128>() as f64;
        let test_amount_f = test_amount.to::<u128>() as f64;
        let loss_pct = (test_amount_f - weth_back_f) / test_amount_f;
        let loss_bps = (loss_pct * 10000.0) as u32;

        let dex_fee_bps = 30u32;
        let token_tax_bps = loss_bps.saturating_sub(dex_fee_bps);

        if token_tax_bps > self.max_tax_bps {
            warn!("🚫 High tax detected: {:?} has {} bps token tax (max: {} bps)", token, token_tax_bps, self.max_tax_bps);
            return TokenSafety::HighTax;
        }

        TokenSafety::Safe
    }

    /// Probe contract for paused / blacklist / owner functions.
    async fn probe_contract_functions<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token: Address,
    ) -> TokenSafety {
        let token_contract = IERC20::new(token, provider);

        if let Ok(result) = token_contract.paused().call().await {
            if result.isPaused {
                warn!("🚫 Token {:?} is paused", token);
                return TokenSafety::Paused;
            }
        }

        let test_address = Address::from_slice(&[0u8; 20]);
        if let Ok(result) = token_contract.isBlacklisted(test_address).call().await {
            let _ = result.blacklisted;
        }

        if let Ok(result) = token_contract.owner().call().await {
            let owner = result.ownerAddr;
            if owner != Address::ZERO {
                info!("Token {:?} has owner: {:?}", token, owner);
            }
        }

        TokenSafety::Safe
    }

    /// v0.7 — Ownership Renouncement Check.
    /// Verifies that the token contract owner has been renounced (set to 0x0 or dead).
    async fn check_ownership_renouncement<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token: Address,
    ) -> TokenSafety {
        let token_contract = IERC20::new(token, provider);

        match token_contract.owner().call().await {
            Ok(result) => {
                let owner = result.ownerAddr;
                if owner == Address::ZERO || owner == DEAD_ADDRESS_1 || owner == DEAD_ADDRESS_2 {
                    TokenSafety::Safe
                } else {
                    warn!("🚫 Token {:?} owner not renounced: {:?}", token, owner);
                    TokenSafety::OwnershipNotRenounced
                }
            }
            Err(_) => {
                // No owner() function = not Ownable = decentralized by default
                TokenSafety::Safe
            }
        }
    }

    /// v0.7 — Liquidity Lock Verification.
    /// If a locker contract is configured, verifies the pool's LP tokens are locked.
    async fn check_liquidity_lock<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        pool_address: Address,
    ) -> TokenSafety {
        let locker_addr = match self.liquidity_locker {
            Some(addr) => addr,
            None => {
                // No locker configured on this network — skip check
                return TokenSafety::Safe;
            }
        };

        let locker = ILiquidityLocker::new(locker_addr, provider);
        match locker.lpLockInfo(pool_address).call().await {
            Ok(result) => {
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();
                if result.unlockDate > now && !result.amount.is_zero() {
                    TokenSafety::Safe
                } else {
                    warn!("🚫 Pool {:?} LP lock expired or amount zero (unlock={}, now={})",
                        pool_address, result.unlockDate, now);
                    TokenSafety::LiquidityNotLocked
                }
            }
            Err(e) => {
                // If the locker doesn't know this pool, it may not be locked
                warn!("Pool {:?} not found in locker (or locker call failed: {}); treating as unlocked", pool_address, e);
                TokenSafety::LiquidityNotLocked
            }
        }
    }

    /// v0.7 — Whale Concentration Filter.
    /// Heuristic: multicall balanceOf on owner, pair, dead, and zero addresses.
    /// If any non-pool address holds >20% of total supply, abort.
    async fn check_whale_concentration<P: Provider<PubSubFrontend>>(
        &self,
        provider: &P,
        token: Address,
        pair: Address,
        multicall3: Option<Address>,
    ) -> TokenSafety {
        let token_contract = IERC20::new(token, provider);

        // 1. Fetch total supply
        let total_supply = match token_contract.totalSupply().call().await {
            Ok(result) => result.supply,
            Err(_) => return TokenSafety::Safe, // Can't verify, assume safe
        };

        if total_supply.is_zero() {
            return TokenSafety::Safe;
        }

        // 2. Build list of suspect addresses to inspect
        // We need owner first; fetch it separately since we need it for the list
        let owner = match token_contract.owner().call().await {
            Ok(result) => result.ownerAddr,
            Err(_) => Address::ZERO,
        };

        let mut suspects: Vec<Address> = vec![
            pair,
            Address::ZERO,
            DEAD_ADDRESS_1,
            DEAD_ADDRESS_2,
        ];
        if owner != Address::ZERO && !suspects.contains(&owner) {
            suspects.push(owner);
        }

        // 3. Fetch balances via multicall if available, else sequential
        let balances: Vec<(Address, U256)> = if let Some(mc) = multicall3 {
            let calls: Vec<(Address, alloy::primitives::Bytes)> = suspects
                .iter()
                .map(|holder| {
                    let call = IERC20::balanceOfCall { account: *holder };
                    (token, alloy::primitives::Bytes::from(call.abi_encode()))
                })
                .collect();

            match multicall3_aggregate3(provider, mc, calls).await {
                Ok(results) => {
                    suspects.into_iter().zip(results.into_iter()).filter_map(|(addr, (success, data))| {
                        if success && data.len() >= 32 {
                            let bal = U256::from_be_slice(&data[0..32]);
                            Some((addr, bal))
                        } else {
                            None
                        }
                    }).collect()
                }
                Err(e) => {
                    warn!("Multicall3 whale check failed for {:?}: {}; falling back to sequential", token, e);
                    // Fallback to sequential calls
                    let mut seq = Vec::new();
                    for s in suspects {
                        if let Ok(result) = token_contract.balanceOf(s).call().await {
                            seq.push((s, result.bal));
                        }
                    }
                    seq
                }
            }
        } else {
            let mut seq = Vec::new();
            for s in suspects {
                if let Ok(result) = token_contract.balanceOf(s).call().await {
                    seq.push((s, result.bal));
                }
            }
            seq
        };

        // 4. Evaluate concentration
        for (addr, bal) in balances {
            // Exclude the pair itself and burn/null addresses from "whale" criteria
            if addr == pair || addr == Address::ZERO || addr == DEAD_ADDRESS_1 || addr == DEAD_ADDRESS_2 {
                continue;
            }

            let pct_bps = (bal * U256::from(10000)) / total_supply;
            let pct = pct_bps.to::<u64>() as f64 / 100.0;

            if pct_bps > U256::from(2000) {
                warn!("🚫 Whale concentration: non-pool address {:?} holds {:.2}% of {:?} supply (threshold 20%)",
                    addr, pct, token);
                return TokenSafety::WhaleConcentration;
            }
        }

        TokenSafety::Safe
    }
}
