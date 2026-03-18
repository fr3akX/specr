use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::LlmClient;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";

// Claude Code version to impersonate when using OAuth tokens.
// OAuth tokens (sk-ant-oat*) are only accepted by Anthropic when the request
// looks like it comes from Claude Code CLI — specific beta headers + user-agent.
const CLAUDE_CODE_VERSION: &str = "2.1.77";

/// Apply auth headers to a reqwest RequestBuilder.
/// - API keys (sk-ant-api*): standard x-api-key header
/// - OAuth tokens (sk-ant-oat*): Bearer auth + Claude Code identity headers
pub(crate) fn apply_auth_headers(
    req: reqwest::RequestBuilder,
    api_key: &str,
) -> reqwest::RequestBuilder {
    if api_key.starts_with("sk-ant-oat") {
        req.header("Authorization", format!("Bearer {}", api_key))
            .header(
                "anthropic-beta",
                "claude-code-20250219,oauth-2025-04-20,fine-grained-tool-streaming-2025-05-14",
            )
            .header(
                "user-agent",
                format!("claude-cli/{CLAUDE_CODE_VERSION} (external)"),
            )
            .header("x-app", "cli")
    } else {
        req.header("x-api-key", api_key)
    }
}

pub struct AnthropicClient {
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl AnthropicClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            http: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct AnthropicRequest {
    model: String,
    max_tokens: u32,
    system: String,
    messages: Vec<Message>,
}

#[derive(Serialize)]
struct Message {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct AnthropicResponse {
    content: Vec<ContentBlock>,
}

#[derive(Deserialize)]
struct ContentBlock {
    text: String,
}

/// Extracts error details from an Anthropic API error response body.
fn format_api_error(status: reqwest::StatusCode, body: &str) -> String {
    // Try to parse as JSON for a cleaner message
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(error) = json.get("error") {
            if let Some(msg) = error.get("message").and_then(|m| m.as_str()) {
                return format!("Anthropic API error ({}): {}", status, msg);
            }
        }
    }
    format!("Anthropic API error ({}): {}", status, body)
}

#[async_trait]
impl LlmClient for AnthropicClient {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let request = AnthropicRequest {
            model: self.model.clone(),
            max_tokens: 4096,
            system: system.to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: user.to_string(),
            }],
        };

        let mut req = self
            .http
            .post(ANTHROPIC_API_URL)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        req = apply_auth_headers(req, &self.api_key);

        let response = req.json(&request).send().await.context(
            "Failed to send request to Anthropic API. Check your network connection and try again.",
        )?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read response body".to_string());
            anyhow::bail!("{}", format_api_error(status, &body));
        }

        let resp: AnthropicResponse = response
            .json()
            .await
            .context("Failed to parse Anthropic API response")?;

        resp.content
            .first()
            .map(|block| block.text.clone())
            .context("Anthropic API returned empty response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_anthropic_client_creation() {
        let client = AnthropicClient::new("sk-test".to_string(), "claude-sonnet-4-6".to_string());
        assert_eq!(client.api_key, "sk-test");
        assert_eq!(client.model, "claude-sonnet-4-6");
    }

    #[test]
    fn test_format_api_error_with_json() {
        let body = r#"{"error": {"type": "invalid_request_error", "message": "Invalid API key"}}"#;
        let result = format_api_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(result.contains("Invalid API key"));
        assert!(result.contains("401"));
    }

    #[test]
    fn test_format_api_error_with_plain_text() {
        let body = "Something went wrong";
        let result = format_api_error(reqwest::StatusCode::INTERNAL_SERVER_ERROR, body);
        assert!(result.contains("Something went wrong"));
        assert!(result.contains("500"));
    }

    #[test]
    fn test_request_serialization() {
        let request = AnthropicRequest {
            model: "claude-sonnet-4-6".to_string(),
            max_tokens: 4096,
            system: "You are helpful".to_string(),
            messages: vec![Message {
                role: "user".to_string(),
                content: "Hello".to_string(),
            }],
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["model"], "claude-sonnet-4-6");
        assert_eq!(json["max_tokens"], 4096);
        assert_eq!(json["system"], "You are helpful");
        assert_eq!(json["messages"][0]["role"], "user");
        assert_eq!(json["messages"][0]["content"], "Hello");
    }

    #[test]
    fn test_response_deserialization() {
        let json = r#"{"content": [{"type": "text", "text": "Hello there"}]}"#;
        let resp: AnthropicResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.content[0].text, "Hello there");
    }

    #[test]
    fn test_response_deserialization_empty() {
        let json = r#"{"content": []}"#;
        let resp: AnthropicResponse = serde_json::from_str(json).unwrap();
        assert!(resp.content.is_empty());
    }
}
