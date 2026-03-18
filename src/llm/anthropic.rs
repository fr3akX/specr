use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::LlmClient;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";

// Claude Code version to impersonate when using OAuth tokens.
// OAuth tokens (sk-ant-oat*) are only accepted by Anthropic when the request
// looks like it comes from Claude Code CLI — specific beta headers + user-agent.
const CLAUDE_CODE_VERSION: &str = "2.1.77";

/// For OAuth tokens, the system field must be an array of content blocks,
/// with a mandatory Claude Code identity prepended. Without it Anthropic
/// returns 400 invalid_request_error.
pub(crate) fn wrap_system_for_oauth(
    api_key: &str,
    mut body: serde_json::Value,
) -> serde_json::Value {
    if !api_key.starts_with("sk-ant-oat") {
        return body;
    }
    let system_text = body
        .get("system")
        .and_then(|s| s.as_str())
        .unwrap_or("")
        .to_string();
    let mut blocks = vec![serde_json::json!({
        "type": "text",
        "text": "You are Claude Code, Anthropic's official CLI for Claude."
    })];
    if !system_text.is_empty() {
        blocks.push(serde_json::json!({"type": "text", "text": system_text}));
    }
    body["system"] = serde_json::json!(blocks);
    body
}

/// Apply auth headers to a reqwest RequestBuilder.
/// - API keys (sk-ant-api*): standard x-api-key header
/// - OAuth tokens (sk-ant-oat*): Bearer auth + Claude Code identity headers
pub(crate) fn apply_auth_headers(
    req: reqwest::RequestBuilder,
    api_key: &str,
) -> reqwest::RequestBuilder {
    if api_key.starts_with("sk-ant-oat") {
        // OAuth tokens require Claude Code identity headers.
        // Note: fine-grained-tool-streaming is omitted — we use non-streaming requests.
        req.header("Authorization", format!("Bearer {}", api_key))
            .header("anthropic-beta", "claude-code-20250219,oauth-2025-04-20")
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
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(error) = json.get("error") {
            let msg = error.get("message").and_then(|m| m.as_str()).unwrap_or("");
            let kind = error
                .get("type")
                .and_then(|t| t.as_str())
                .unwrap_or("unknown");
            // If message is empty or suspiciously short, include the full body
            if msg.len() > 3 {
                return format!("Anthropic API error ({} {}): {}", status, kind, msg);
            }
        }
    }
    // Fallback: show full body so we can diagnose
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

        // OAuth tokens require system as content blocks with a mandatory
        // Claude Code identity block prepended — plain string system is rejected.
        let request_value =
            serde_json::to_value(&request).context("Failed to serialize request")?;
        let request_value = wrap_system_for_oauth(&self.api_key, request_value);

        let response = req
            .json(&request_value)
            .send()
            .await
            .with_context(|| "Failed to connect to Anthropic API (check network/TLS)")?;

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
