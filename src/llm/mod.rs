pub mod anthropic;
pub mod claude_cli;
pub mod openai;

use anyhow::Result;
use async_trait::async_trait;

use crate::config::Config;

/// Trait for LLM providers. Each provider implements a simple completion interface.
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, system: &str, user: &str) -> Result<String>;
}

/// Create the appropriate LLM client based on config.
///
/// For `claude-cli` provider, no API key is needed — it uses the `claude`
/// binary already configured on the system (Claude Code subscription).
/// Pass an empty string for `api_key` in that case.
pub fn create_client(config: &Config, api_key: &str) -> Result<Box<dyn LlmClient>> {
    match config.llm.provider.as_str() {
        "anthropic" => Ok(Box::new(anthropic::AnthropicClient::new(
            api_key.to_string(),
            config.llm.model.clone(),
        ))),
        "openai" => Ok(Box::new(openai::OpenAIClient::new(
            api_key.to_string(),
            config.llm.model.clone(),
        ))),
        "claude-cli" => {
            let bin = config.agent.runner_bin.clone();
            // Use model from llm config if set, otherwise let the CLI use its default
            let model = if config.llm.model.is_empty() {
                None
            } else {
                Some(config.llm.model.clone())
            };
            Ok(Box::new(claude_cli::ClaudeCliClient::new(bin, model)))
        }
        other => Err(anyhow::anyhow!(
            "Unknown LLM provider: {other}. Use 'anthropic', 'openai', or 'claude-cli'."
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn make_config(provider: &str) -> Config {
        let mut config = Config::default();
        config.llm.provider = provider.to_string();
        config.llm.model = "test-model".to_string();
        config.llm.api_key_env = "TEST_KEY".to_string();
        config
    }

    #[test]
    fn test_create_anthropic_client() {
        let config = make_config("anthropic");
        let client = create_client(&config, "sk-test");
        assert!(client.is_ok());
    }

    #[test]
    fn test_create_openai_client() {
        let config = make_config("openai");
        let client = create_client(&config, "sk-test");
        assert!(client.is_ok());
    }

    #[test]
    fn test_create_claude_cli_client() {
        let config = make_config("claude-cli");
        let client = create_client(&config, "");
        assert!(client.is_ok());
    }

    #[test]
    fn test_create_unknown_client() {
        let config = make_config("gemini");
        let result = create_client(&config, "sk-test");
        assert!(result.is_err());
        let err = result.err().unwrap();
        assert!(err.to_string().contains("Unknown LLM provider"));
    }
}
