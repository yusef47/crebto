use alloy::{
    providers::{ProviderBuilder, RootProvider},
    transports::http::Http,
};
use reqwest::Client;
use std::{
    sync::atomic::{AtomicUsize, Ordering},
    time::Instant,
};
use tokio::sync::RwLock;
use tracing::{debug, warn};

/// Rotating HTTP provider for free-tier RPCs on Base.
/// Cycles through endpoints with exponential backoff on rate limits.
pub struct RotatingProvider {
    endpoints: Vec<String>,
    providers: Vec<RootProvider<Http<Client>>>,
    current_index: AtomicUsize,
    last_failure: RwLock<Vec<Instant>>,
    backoff_secs: Vec<AtomicUsize>,
}

impl RotatingProvider {
    pub async fn new(urls: Vec<String>) -> eyre::Result<Self> {
        let mut providers = Vec::with_capacity(urls.len());
        let mut backoffs = Vec::with_capacity(urls.len());

        for url in &urls {
            match url.parse::<reqwest::Url>() {
                Ok(parsed) => {
                    let provider = ProviderBuilder::new().on_http(parsed);
                    providers.push(provider);
                    backoffs.push(AtomicUsize::new(1));
                }
                Err(e) => {
                    warn!("Failed to parse RPC URL {}: {}", url, e);
                    continue;
                }
            }
        }

        let n = urls.len();
        if providers.is_empty() {
            return Err(eyre::eyre!("No valid RPC URLs provided"));
        }

        Ok(Self {
            endpoints: urls,
            providers,
            current_index: AtomicUsize::new(0),
            last_failure: RwLock::new(vec![Instant::now(); n]),
            backoff_secs: backoffs,
        })
    }

    /// Execute an RPC call, rotating on failure.
    pub async fn call_with_fallback<F, Fut, T>(&self, f: F) -> eyre::Result<T>
    where
        F: Fn(&RootProvider<Http<Client>>) -> Fut,
        Fut: std::future::Future<Output = eyre::Result<T>>,
    {
        let n = self.providers.len();
        let start = self.current_index.load(Ordering::Relaxed);

        for offset in 1..=n {
            let idx = (start + offset) % n;

            let last_fail = self.last_failure.read().await[idx];
            let backoff = self.backoff_secs[idx].load(Ordering::Relaxed);
            if last_fail.elapsed().as_secs() < backoff as u64 {
                debug!("RPC endpoint {} in backoff ({}s)", idx, backoff);
                continue;
            }

            match f(&self.providers[idx]).await {
                Ok(result) => {
                    self.backoff_secs[idx].store(2, Ordering::Relaxed);
                    self.current_index.store(idx, Ordering::Relaxed);
                    return Ok(result);
                }
                Err(e) => {
                    let msg = format!("{}", e);
                    let is_rate_limit = msg.contains("429")
                        || msg.contains("-32000")
                        || msg.to_lowercase().contains("rate limit")
                        || msg.to_lowercase().contains("too many requests")
                        || msg.to_lowercase().contains("exceeded");

                    let mut failures = self.last_failure.write().await;
                    failures[idx] = Instant::now();
                    drop(failures);

                    if is_rate_limit {
                        let current = self.backoff_secs[idx].load(Ordering::Relaxed);
                        let next = (current * 2).min(60);
                        self.backoff_secs[idx].store(next, Ordering::Relaxed);
                        warn!(
                            "RPC {} rate-limited. Backoff {}s. Error: {}",
                            self.endpoints.get(idx).unwrap_or(&"?".to_string()),
                            next,
                            msg
                        );
                    } else {
                        warn!("RPC {} failed: {}. Rotating...", self.endpoints.get(idx).unwrap_or(&"?".to_string()), msg);
                    }
                }
            }
        }

        Err(eyre::eyre!("All {} RPC endpoints exhausted", n))
    }

    pub fn best_provider(&self) -> &RootProvider<Http<Client>> {
        let idx = self.current_index.load(Ordering::Relaxed);
        &self.providers[idx % self.providers.len()]
    }

    pub fn best_provider_owned(&self) -> RootProvider<Http<Client>> {
        let idx = self.current_index.load(Ordering::Relaxed);
        // Clone the provider (RootProvider<Http<Client>> is Clone)
        self.providers[idx % self.providers.len()].clone()
    }
}
