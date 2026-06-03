mod config;
mod dex;
mod stream;
mod simulator;
mod strategy;
mod executor;
mod alerts;

use config::Config;
use stream::{WsListener, SwapDecoder, DecodedSwap};
use simulator::{TxSimulator, SafetyChecker};
use strategy::{PathFinder, AmountOptimizer, ArbPath};
use executor::{NonceManager, TxBuilder};
use alerts::TelegramNotifier;

use alloy::{
    providers::{Provider, ProviderBuilder, WsConnect},
    rpc::types::eth::Log,
    primitives::{Address, U256},
};
use tokio::sync::mpsc;
use tracing::{info, warn, error, Level};
use tracing_subscriber::FmtSubscriber;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

#[derive(Debug)]
pub struct BotState {
    pub consecutive_failures: Arc<AtomicU32>,
}

#[tokio::main]
async fn main() -> Result<(), eyre::Report> {
    // 1. Initialize Logging
    let subscriber = FmtSubscriber::builder()
        .with_max_level(Level::INFO)
        .finish();
    tracing::subscriber::set_global_default(subscriber)?;

    info!("Starting Crebto Arbitrage Bot...");

    // 2. Load Configuration
    let config = Config::load_from_env()?;
    info!("Configuration loaded. Target contract: {:?}", config.contract_address);

    // 3. Setup Telegram Notifications
    let notifier = if let (Some(token), Some(chat_id)) = (&config.telegram_bot_token, &config.telegram_chat_id) {
        let n = TelegramNotifier::new(token.clone(), chat_id.clone());
        n.send_message("🚀 *Crebto Bot Started* on Base L2!").await;
        Some(n)
    } else {
        warn!("Telegram notifier parameters not fully set. Alerts disabled.");
        None
    };

    // 4. Initialize Blockchain Providers (Alloy)
    let ws = WsConnect::new(&config.alchemy_wss);
    let provider = ProviderBuilder::new().on_ws(ws).await?;
    let provider = Arc::new(provider);

    // Fetch initial wallet nonce
    let signer = config.private_key.parse::<alloy::signers::local::PrivateKeySigner>()?;
    let wallet_address = signer.address();
    info!("Wallet address: {:?}", wallet_address);
    
    let nonce_manager = NonceManager::initialize(&provider, wallet_address).await?;
    let nonce_manager = Arc::new(nonce_manager);

    // 5. Initialize Tx Simulator & Builder
    let mut simulator = TxSimulator::new();
    let tx_builder = TxBuilder::new(config.contract_address, &config.private_key)?;

    // 6. Setup Channels
    let (log_tx, mut log_rx) = mpsc::channel::<Log>(500);
    let (block_tx, mut block_rx) = mpsc::channel::<u64>(50);

    // 7. Start WebSocket Listener (Collector)
    let listener = WsListener::new(config.alchemy_wss.clone());
    listener.listen(log_tx, block_tx).await?;

    // Bot State for Kill Switch
    let state = BotState {
        consecutive_failures: Arc::new(AtomicU32::new(0)),
    };

    // Instantiate DEX Quoters
    let uni_quoter = dex::uniswap_v3::UniswapV3Quoter::new(
        Address::parse_checksummed("0x2626664c2603336E57B271c5C0b26F421741e481", None).unwrap()
    );
    let aero_quoter = dex::aerodrome::AerodromeQuoter::new(
        Address::parse_checksummed("0xcF77a3Ba9A5CA399B7c97c74d54e5b1Beb874E43", None).unwrap(),
        Address::parse_checksummed("0x420DD381b31aEf6683db6B902084cB0FFECe40Da", None).unwrap(),
        false
    );

    info!("Bot engine event loop is running...");

    // 8. Event Loop
    loop {
        // Check Kill Switch conditions
        let failures = state.consecutive_failures.load(Ordering::SeqCst);
        if failures >= config.max_consecutive_failures {
            let msg = format!("🚨 *KILL SWITCH TRIGGERED*: Bot encountered {} consecutive failures. Shutting down.", failures);
            error!("{}", msg);
            if let Some(ref n) = notifier {
                n.send_message(&msg).await;
            }
            break;
        }

        tokio::select! {
            // New Blocks (Flashblocks / Block updates)
            Some(block_number) = block_rx.recv() => {
                info!("New block received: {}", block_number);
                // In production, we'd sync token prices / pool reserves here if needed
            }

            // Incoming Swap Logs
            Some(log) = log_rx.recv() => {
                let decoded = match SwapDecoder::decode(&log) {
                    Ok(d) => d,
                    Err(_) => continue, // Ignore unparsable/irrelevant swaps
                };

                match decoded {
                    DecodedSwap::UniswapV3 { pool, amount0, amount1, .. } => {
                        info!("Uniswap V3 Swap event on pool: {:?}", pool);
                        // In production: trigger pathfinder to check for opportunities
                    }
                    DecodedSwap::Aerodrome { pool, amount0_in, amount1_in, .. } => {
                        info!("Aerodrome Swap event on pool: {:?}", pool);
                    }
                }
            }
        }
    }

    Ok(())
}
