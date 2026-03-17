use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Top-level configuration loaded from ~/.config/specr/config.toml.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub llm: LlmConfig,
    pub output: OutputConfig,
    pub spec: SpecConfig,
    #[serde(default)]
    pub agent: AgentConfig,
    #[serde(default)]
    pub telegram: TelegramConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LlmConfig {
    pub provider: String,
    /// Model name or alias. For `claude-cli`, this maps to `claude --model <model>`.
    pub model: String,
    pub api_key_env: String,
    /// Timeout (seconds) for the coding agent subprocess (claude CLI).
    #[serde(default = "default_llm_timeout_seconds")]
    pub timeout_seconds: u64,
    /// Timeout (seconds) for each review LLM call. Defaults to 120s.
    #[serde(default = "default_review_timeout_seconds")]
    pub review_timeout_seconds: u64,
    /// Model to use for review agents. Defaults to same as `model`.
    /// Set to a faster/cheaper model (e.g. "haiku") to speed up reviews.
    #[serde(default)]
    pub review_model: String,
    /// Provider for review agents. Defaults to same as `provider`.
    /// Set to "anthropic" to use direct API for reviews while using claude-cli for coding.
    #[serde(default)]
    pub review_provider: String,
    /// Env var holding the API key for review agents.
    /// Defaults to same as `api_key_env` if not set.
    #[serde(default)]
    pub review_api_key_env: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutputConfig {
    pub base_dir: String,
    pub obsidian_dir: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SpecConfig {
    pub question_budget: usize,
    #[serde(default = "default_max_loop_iterations")]
    pub max_loop_iterations: u32,
}

fn default_max_loop_iterations() -> u32 {
    5
}

fn default_llm_timeout_seconds() -> u64 {
    300
}

fn default_review_timeout_seconds() -> u64 {
    120
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentConfig {
    pub runner: String,
    pub runner_bin: String,
    pub stream_output: bool,
    /// Show LLM reasoning (approach plan) before running the coding agent. Default: true.
    #[serde(default = "default_true")]
    pub show_reasoning: bool,
    /// Max tool-use turns for the "api-agent" runner. Default: 30.
    #[serde(default = "default_max_agent_turns")]
    pub max_agent_turns: u32,
}

fn default_max_agent_turns() -> u32 {
    30
}

impl Default for AgentConfig {
    fn default() -> Self {
        AgentConfig {
            runner: "claude-code".to_string(),
            runner_bin: "claude".to_string(),
            stream_output: true,
            show_reasoning: true,
            max_agent_turns: default_max_agent_turns(),
        }
    }
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TelegramConfig {
    pub bot_token_env: String,
    pub chat_id_env: String,
    pub enabled: bool,
    pub autostart: bool,
}

impl Default for TelegramConfig {
    fn default() -> Self {
        TelegramConfig {
            bot_token_env: "TELEGRAM_BOT_TOKEN".to_string(),
            chat_id_env: "TELEGRAM_CHAT_ID".to_string(),
            enabled: false,
            autostart: false,
        }
    }
}

impl Default for Config {
    fn default() -> Self {
        Config {
            llm: LlmConfig {
                provider: "claude-cli".to_string(),
                model: "sonnet".to_string(),
                api_key_env: "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                timeout_seconds: default_llm_timeout_seconds(),
                review_timeout_seconds: default_review_timeout_seconds(),
                review_model: String::new(), // empty = use same as model
                review_provider: String::new(), // empty = use same as provider
                review_api_key_env: String::new(), // empty = use same as api_key_env
            },
            output: OutputConfig {
                base_dir: ".".to_string(),
                obsidian_dir: String::new(),
            },
            spec: SpecConfig {
                question_budget: 8,
                max_loop_iterations: 5,
            },
            agent: AgentConfig::default(),
            telegram: TelegramConfig::default(),
        }
    }
}

/// Returns the path to the config file: ~/.config/specr/config.toml
pub fn config_path() -> Result<PathBuf> {
    let config_dir = dirs::config_dir().context("Could not determine config directory")?;
    Ok(config_dir.join("specr").join("config.toml"))
}

/// Load config from the given path, creating it with defaults if missing.
pub fn load_config_from(path: &Path) -> Result<Config> {
    if path.exists() {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read config from {}", path.display()))?;
        let config: Config = toml::from_str(&content)
            .with_context(|| format!("Failed to parse config from {}", path.display()))?;
        Ok(config)
    } else {
        let config = Config::default();
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).with_context(|| {
                format!("Failed to create config directory {}", parent.display())
            })?;
        }
        let content =
            toml::to_string_pretty(&config).context("Failed to serialize default config")?;
        std::fs::write(path, &content)
            .with_context(|| format!("Failed to write default config to {}", path.display()))?;
        Ok(config)
    }
}

/// Load config from the default path (~/.config/specr/config.toml).
pub fn load_config() -> Result<Config> {
    let path = config_path()?;
    load_config_from(&path)
}

/// Resolve the API key by reading the environment variable named in config.
pub fn resolve_api_key(config: &Config) -> Result<String> {
    let env_name = &config.llm.api_key_env;
    std::env::var(env_name).with_context(|| {
        format!(
            "Set {} in your environment (configured via llm.api_key_env in config)",
            env_name
        )
    })
}

/// Resolve the API key, returning an empty string for providers that don't need one (e.g. claude-cli).
pub fn resolve_api_key_optional(config: &Config) -> Result<String> {
    if config.llm.provider == "claude-cli" {
        return Ok(String::new());
    }
    resolve_api_key(config)
}

// Resolve API key for review calls.
// Priority:
// Resolve API key for review calls.
// Priority: 1) review_api_key_env, 2) api_key_env. Returns "" for claude-cli.
pub fn resolve_review_api_key(config: &Config) -> Result<String> {
    let effective_provider = if config.llm.review_provider.is_empty() {
        &config.llm.provider
    } else {
        &config.llm.review_provider
    };

    if effective_provider == "claude-cli" {
        return Ok(String::new());
    }

    // 1. Explicit review key env var
    if !config.llm.review_api_key_env.is_empty() {
        if let Ok(key) = std::env::var(&config.llm.review_api_key_env) {
            if !key.is_empty() {
                return Ok(key);
            }
        }
    }

    // 2. Shared api_key_env
    if let Ok(key) = std::env::var(&config.llm.api_key_env) {
        if !key.is_empty() {
            return Ok(key);
        }
    }

    let env_name = if config.llm.review_api_key_env.is_empty() {
        &config.llm.api_key_env
    } else {
        &config.llm.review_api_key_env
    };
    Err(anyhow::anyhow!(
        "No API key found for review provider '{effective_provider}'. Set {env_name} in your environment."
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_default_config() {
        let config = Config::default();
        assert_eq!(config.llm.provider, "claude-cli");
        assert_eq!(config.llm.model, "sonnet");
        assert_eq!(config.llm.api_key_env, "CLAUDE_CODE_OAUTH_TOKEN");
        assert_eq!(config.llm.timeout_seconds, 300);
        assert_eq!(config.llm.review_timeout_seconds, 120);
        assert_eq!(config.output.base_dir, ".");
        assert!(config.output.obsidian_dir.is_empty());
        assert_eq!(config.spec.question_budget, 8);
        assert_eq!(config.spec.max_loop_iterations, 5);
        assert_eq!(config.agent.runner, "claude-code");
        assert_eq!(config.agent.runner_bin, "claude");
        assert!(config.agent.stream_output);
        assert!(!config.telegram.enabled);
        assert!(!config.telegram.autostart);
        assert_eq!(config.telegram.bot_token_env, "TELEGRAM_BOT_TOKEN");
        assert_eq!(config.telegram.chat_id_env, "TELEGRAM_CHAT_ID");
    }

    #[test]
    fn test_load_config_creates_default_if_missing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");
        assert!(!path.exists());

        let config = load_config_from(&path).unwrap();
        assert!(path.exists());
        assert_eq!(config.llm.provider, "claude-cli");
        assert_eq!(config.llm.timeout_seconds, 300);
    }

    #[test]
    fn test_load_config_reads_existing() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("config.toml");

        let content = r#"
[llm]
provider = "openai"
model = "gpt-4"
api_key_env = "OPENAI_API_KEY"
timeout_seconds = 123

[output]
base_dir = "./output"
obsidian_dir = "/vault"

[spec]
question_budget = 5
"#;
        std::fs::write(&path, content).unwrap();

        let config = load_config_from(&path).unwrap();
        assert_eq!(config.llm.provider, "openai");
        assert_eq!(config.llm.model, "gpt-4");
        assert_eq!(config.llm.api_key_env, "OPENAI_API_KEY");
        assert_eq!(config.llm.timeout_seconds, 123);
        assert_eq!(config.output.base_dir, "./output");
        assert_eq!(config.output.obsidian_dir, "/vault");
        assert_eq!(config.spec.question_budget, 5);
    }

    #[test]
    fn test_load_config_nested_dir_creation() {
        let tmp = TempDir::new().unwrap();
        let path = tmp.path().join("a").join("b").join("config.toml");
        let config = load_config_from(&path).unwrap();
        assert_eq!(config.llm.provider, "claude-cli");
        assert_eq!(config.llm.timeout_seconds, 300);
        assert!(path.exists());
    }

    #[test]
    fn test_resolve_api_key_success() {
        let config = Config::default();
        // Use a unique env var name to avoid conflicts
        let mut config = config;
        config.llm.api_key_env = "SPECR_TEST_API_KEY_12345".to_string();
        std::env::set_var("SPECR_TEST_API_KEY_12345", "sk-test-123");
        let key = resolve_api_key(&config).unwrap();
        assert_eq!(key, "sk-test-123");
        std::env::remove_var("SPECR_TEST_API_KEY_12345");
    }

    #[test]
    fn test_resolve_api_key_missing() {
        let mut config = Config::default();
        config.llm.api_key_env = "SPECR_NONEXISTENT_KEY_XYZ_99999".to_string();
        std::env::remove_var("SPECR_NONEXISTENT_KEY_XYZ_99999");
        let result = resolve_api_key(&config);
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("SPECR_NONEXISTENT_KEY_XYZ_99999"));
    }

    #[test]
    fn test_config_roundtrip_serialization() {
        let config = Config::default();
        let serialized = toml::to_string_pretty(&config).unwrap();
        let deserialized: Config = toml::from_str(&serialized).unwrap();
        assert_eq!(deserialized.llm.provider, config.llm.provider);
        assert_eq!(deserialized.llm.model, config.llm.model);
        assert_eq!(
            deserialized.spec.question_budget,
            config.spec.question_budget
        );
    }
}
