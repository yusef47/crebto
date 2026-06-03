use reqwest::Client;
use serde_json::json;
use tracing::{info, error};

#[derive(Clone)]
pub struct TelegramNotifier {
    client: Client,
    bot_token: String,
    chat_id: String,
}

impl TelegramNotifier {
    pub fn new(bot_token: String, chat_id: String) -> Self {
        Self {
            client: Client::new(),
            bot_token,
            chat_id,
        }
    }

    /// Sends a text message to the designated Telegram chat
    pub async fn send_message(&self, text: &str) {
        let url = format!("https://api.telegram.org/bot{}/sendMessage", self.bot_token);
        
        let payload = json!({
            "chat_id": self.chat_id,
            "text": text,
            "parse_mode": "Markdown"
        });

        match self.client.post(&url).json(&payload).send().await {
            Ok(resp) => {
                if resp.status().is_success() {
                    info!("Telegram message sent successfully!");
                } else {
                    error!("Telegram server returned error: {:?}", resp.text().await);
                }
            }
            Err(e) => {
                error!("Failed to send message to Telegram: {:?}", e);
            }
        }
    }
}
