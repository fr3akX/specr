use anyhow::{Context, Result};
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::LlmClient;

const OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";

pub struct OpenAIClient {
    api_key: String,
    model: String,
    http: reqwest::Client,
}

impl OpenAIClient {
    pub fn new(api_key: String, model: String) -> Self {
        Self {
            api_key,
            model,
            http: reqwest::Client::new(),
        }
    }
}

#[derive(Serialize)]
struct ChatCompletionRequest {
    model: String,
    messages: Vec<ChatMessage>,
    max_tokens: u32,
}

#[derive(Serialize)]
struct ChatMessage {
    role: String,
    content: String,
}

#[derive(Deserialize)]
struct ChatCompletionResponse {
    choices: Vec<Choice>,
}

#[derive(Deserialize)]
struct Choice {
    message: ResponseMessage,
}

#[derive(Deserialize)]
struct ResponseMessage {
    content: String,
}

/// Extracts error details from an OpenAI API error response body.
fn format_api_error(status: reqwest::StatusCode, body: &str) -> String {
    if let Ok(json) = serde_json::from_str::<serde_json::Value>(body) {
        if let Some(error) = json.get("error") {
            if let Some(msg) = error.get("message").and_then(|m| m.as_str()) {
                return format!("OpenAI API error ({}): {}", status, msg);
            }
        }
    }
    format!("OpenAI API error ({}): {}", status, body)
}

#[async_trait]
impl LlmClient for OpenAIClient {
    async fn complete(&self, system: &str, user: &str) -> Result<String> {
        let request = ChatCompletionRequest {
            model: self.model.clone(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: system.to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: user.to_string(),
                },
            ],
            max_tokens: 4096,
        };

        let response = self
            .http
            .post(OPENAI_API_URL)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&request)
            .send()
            .await
            .context("Failed to send request to OpenAI API. Check your network connection and try again.")?;

        let status = response.status();
        if !status.is_success() {
            let body = response
                .text()
                .await
                .unwrap_or_else(|_| "Unable to read response body".to_string());
            anyhow::bail!("{}", format_api_error(status, &body));
        }

        let resp: ChatCompletionResponse = response
            .json()
            .await
            .context("Failed to parse OpenAI API response")?;

        resp.choices
            .first()
            .map(|choice| choice.message.content.clone())
            .context("OpenAI API returned empty response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_openai_client_creation() {
        let client = OpenAIClient::new("sk-test".to_string(), "gpt-4".to_string());
        assert_eq!(client.api_key, "sk-test");
        assert_eq!(client.model, "gpt-4");
    }

    #[test]
    fn test_format_api_error_with_json() {
        let body = r#"{"error": {"message": "Incorrect API key", "type": "invalid_request_error"}}"#;
        let result = format_api_error(reqwest::StatusCode::UNAUTHORIZED, body);
        assert!(result.contains("Incorrect API key"));
        assert!(result.contains("401"));
    }

    #[test]
    fn test_format_api_error_with_plain_text() {
        let body = "Bad gateway";
        let result = format_api_error(reqwest::StatusCode::BAD_GATEWAY, body);
        assert!(result.contains("Bad gateway"));
        assert!(result.contains("502"));
    }

    #[test]
    fn test_request_serialization() {
        let request = ChatCompletionRequest {
            model: "gpt-4".to_string(),
            messages: vec![
                ChatMessage {
                    role: "system".to_string(),
                    content: "Be helpful".to_string(),
                },
                ChatMessage {
                    role: "user".to_string(),
                    content: "Hello".to_string(),
                },
            ],
            max_tokens: 4096,
        };
        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["model"], "gpt-4");
        assert_eq!(json["max_tokens"], 4096);
        assert_eq!(json["messages"].as_array().unwrap().len(), 2);
        assert_eq!(json["messages"][0]["role"], "system");
        assert_eq!(json["messages"][1]["role"], "user");
    }

    #[test]
    fn test_response_deserialization() {
        let json = r#"{"choices": [{"message": {"role": "assistant", "content": "Hi!"}, "finish_reason": "stop", "index": 0}]}"#;
        let resp: ChatCompletionResponse = serde_json::from_str(json).unwrap();
        assert_eq!(resp.choices[0].message.content, "Hi!");
    }

    #[test]
    fn test_response_deserialization_empty() {
        let json = r#"{"choices": []}"#;
        let resp: ChatCompletionResponse = serde_json::from_str(json).unwrap();
        assert!(resp.choices.is_empty());
    }
}
