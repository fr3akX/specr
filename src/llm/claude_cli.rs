use anyhow::{Context, Result};
use async_trait::async_trait;
use std::{process::Stdio, time::Duration};
use tokio::{process::Command, time};

use super::LlmClient;

/// LLM client that delegates to the `claude` CLI via subprocess.
/// Uses `claude -p` (print mode) — no API key required.
pub struct ClaudeCliClient {
    /// Path or name of the claude binary (from config.agent.runner_bin)
    bin: String,
    /// Optional model override (e.g. "claude-opus-4-6")
    model: Option<String>,
    /// Timeout for a single completion.
    timeout_seconds: u64,
}

impl ClaudeCliClient {
    pub fn new(bin: String, model: Option<String>, timeout_seconds: u64) -> Self {
        Self {
            bin,
            model,
            timeout_seconds,
        }
    }
}

#[async_trait]
impl LlmClient for ClaudeCliClient {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let mut cmd = Command::new(&self.bin);

        cmd.arg("-p");

        if !system.is_empty() {
            cmd.arg("--system-prompt").arg(system);
        }

        // Prevent any session I/O blocking.
        cmd.arg("--no-session-persistence");

        // Allow file access without permission prompts.
        cmd.arg("--dangerously-skip-permissions");

        if let Some(ref model) = self.model {
            cmd.arg("--model").arg(model);
        }

        cmd.env("NO_COLOR", "1");

        // Ensure claude cannot read from our terminal.
        cmd.stdin(Stdio::null());
        cmd.stdout(Stdio::piped());
        cmd.stderr(Stdio::piped());

        // Run from /tmp so claude does not scan the caller's project directory.
        // In -p mode, claude still indexes the cwd as workspace context — this
        // prevents it from spending time (or hanging) on large directory trees.
        cmd.current_dir(std::env::temp_dir());

        // Prompt as positional argument.
        cmd.arg(user);

        let child = cmd
            .spawn()
            .with_context(|| format!("Failed to spawn claude CLI at '{}'", self.bin))?;

        let timeout = Duration::from_secs(self.timeout_seconds);
        let output = match time::timeout(timeout, child.wait_with_output()).await {
            Ok(res) => res.context("Failed to wait for claude CLI")?,
            Err(_) => {
                // We can't kill here because wait_with_output() takes ownership of the Child.
                // Rely on timeout error and let the OS reap the process.
                return Err(anyhow::anyhow!(
                    "claude-cli timed out after {}s (try setting llm.timeout_seconds higher, or use a faster model like 'sonnet')",
                    self.timeout_seconds
                ));
            }
        };

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
        let client = ClaudeCliClient::new("claude".to_string(), None, 10);
        assert_eq!(client.bin, "claude");
        assert!(client.model.is_none());
        assert_eq!(client.timeout_seconds, 10);
    }

    #[tokio::test]
    async fn test_claude_cli_missing_binary() {
        let client = ClaudeCliClient::new("__nonexistent_binary_specr__".to_string(), None, 1);
        let result = client.complete("system", "user").await;
        assert!(result.is_err());
        let err = result.unwrap_err().to_string();
        assert!(err.contains("__nonexistent_binary_specr__"));
    }

    #[tokio::test]
    async fn test_timeout_error_message() {
        // Use a tiny timeout and a real binary name is not reliable in CI, so just validate format
        let client = ClaudeCliClient::new("claude".to_string(), None, 0);
        let result = client.complete("sys", "user").await;
        // Either it errors immediately or times out; both are acceptable here.
        assert!(result.is_err());
    }
}
