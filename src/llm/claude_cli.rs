use anyhow::{Context, Result};
use async_trait::async_trait;
use tokio::process::Command;

use super::LlmClient;

/// LLM client that delegates to the `claude` CLI via subprocess.
/// Uses `claude -p "<prompt>"` (print mode) — no API key required,
/// uses the Claude Code subscription already configured on the system.
pub struct ClaudeCliClient {
    /// Path or name of the claude binary (from config.agent.runner_bin)
    bin: String,
    /// Optional model override (e.g. "claude-opus-4-6")
    model: Option<String>,
}

impl ClaudeCliClient {
    pub fn new(bin: String, model: Option<String>) -> Self {
        Self { bin, model }
    }
}

#[async_trait]
impl LlmClient for ClaudeCliClient {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        // Combine system and user prompts into a single prompt for the CLI.
        // claude -p accepts a single prompt string; we prepend the system prompt
        // as a clearly delimited block so the model sees full context.
        let combined = format!(
            "<system>\n{system}\n</system>\n\n{user}",
            system = system,
            user = user
        );

        let mut cmd = Command::new(&self.bin);
        cmd.arg("--print").arg(&combined);

        if let Some(ref model) = self.model {
            cmd.arg("--model").arg(model);
        }

        // Suppress interactive UI — we only want stdout
        cmd.env("NO_COLOR", "1");

        let output = cmd
            .output()
            .await
            .with_context(|| format!("Failed to spawn claude CLI at '{}'", self.bin))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!(
                "claude CLI exited with status {}: {}",
                output.status,
                stderr.trim()
            ));
        }

        let response =
            String::from_utf8(output.stdout).context("claude CLI returned non-UTF8 output")?;

        Ok(response.trim().to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_claude_cli_client_creation() {
        let client = ClaudeCliClient::new("claude".to_string(), None);
        assert_eq!(client.bin, "claude");
        assert!(client.model.is_none());
    }

    #[test]
    fn test_claude_cli_client_with_model() {
        let client =
            ClaudeCliClient::new("claude".to_string(), Some("claude-opus-4-6".to_string()));
        assert_eq!(client.bin, "claude");
        assert_eq!(client.model.as_deref(), Some("claude-opus-4-6"));
    }

    #[tokio::test]
    async fn test_claude_cli_missing_binary() {
        let client = ClaudeCliClient::new("__nonexistent_binary_specr__".to_string(), None);
        let result = client.complete("system", "user").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("__nonexistent_binary_specr__"));
    }
}
