use anyhow::{Context, Result};

/// Sends notifications via the Telegram Bot API.
pub struct TelegramNotifier {
    bot_token: Option<String>,
    chat_id: Option<String>,
    enabled: bool,
}

impl TelegramNotifier {
    pub fn new(bot_token: String, chat_id: String) -> Self {
        TelegramNotifier {
            bot_token: Some(bot_token),
            chat_id: Some(chat_id),
            enabled: true,
        }
    }

    /// Create a disabled notifier (no-op).
    pub fn disabled() -> Self {
        TelegramNotifier {
            bot_token: None,
            chat_id: None,
            enabled: false,
        }
    }

    /// Create from config, resolving env vars. Returns disabled if not configured.
    pub fn from_config(config: &crate::config::Config) -> Self {
        if !config.telegram.enabled {
            return Self::disabled();
        }

        let bot_token = std::env::var(&config.telegram.bot_token_env).ok();
        let chat_id = std::env::var(&config.telegram.chat_id_env).ok();

        match (bot_token, chat_id) {
            (Some(token), Some(id)) => Self::new(token, id),
            _ => {
                eprintln!(
                    "Warning: Telegram enabled but {} or {} not set",
                    config.telegram.bot_token_env, config.telegram.chat_id_env
                );
                Self::disabled()
            }
        }
    }

    /// Send a message. No-op if disabled.
    pub async fn send(&self, message: &str) -> Result<()> {
        if !self.enabled {
            return Ok(());
        }

        let bot_token = self.bot_token.as_ref().context("Missing bot token")?;
        let chat_id = self.chat_id.as_ref().context("Missing chat ID")?;

        let url = format!(
            "https://api.telegram.org/bot{}/sendMessage",
            bot_token
        );

        let client = reqwest::Client::new();
        let resp = client
            .post(&url)
            .json(&serde_json::json!({
                "chat_id": chat_id,
                "text": message,
                "parse_mode": "HTML"
            }))
            .send()
            .await
            .context("Failed to send Telegram message")?;

        if !resp.status().is_success() {
            let body = resp.text().await.unwrap_or_default();
            anyhow::bail!("Telegram API error: {}", body);
        }

        Ok(())
    }

    #[allow(dead_code)]
    pub fn is_enabled(&self) -> bool {
        self.enabled
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_disabled_notifier() {
        let notifier = TelegramNotifier::disabled();
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_enabled_notifier() {
        let notifier = TelegramNotifier::new("token".to_string(), "chat".to_string());
        assert!(notifier.is_enabled());
    }

    #[tokio::test]
    async fn test_disabled_send_is_noop() {
        let notifier = TelegramNotifier::disabled();
        let result = notifier.send("test message").await;
        assert!(result.is_ok());
    }

    #[test]
    fn test_from_config_disabled() {
        let config = crate::config::Config::default();
        // telegram.enabled = false by default
        let notifier = TelegramNotifier::from_config(&config);
        assert!(!notifier.is_enabled());
    }

    #[test]
    fn test_from_config_enabled_missing_env() {
        let mut config = crate::config::Config::default();
        config.telegram.enabled = true;
        config.telegram.bot_token_env = "SPECR_TEST_TG_TOKEN_NONEXIST".to_string();
        config.telegram.chat_id_env = "SPECR_TEST_TG_CHAT_NONEXIST".to_string();
        let notifier = TelegramNotifier::from_config(&config);
        assert!(!notifier.is_enabled());
    }
}
