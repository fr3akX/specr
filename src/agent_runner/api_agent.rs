//! API-based coding agent using Anthropic's tool-use API.
//!
//! Opt-in alternative to claude-cli. Set `agent.runner = "api-agent"` in config.
//! Uses CLAUDE_CODE_OAUTH_TOKEN (or api_key_env) for auth.
//!
//! The agent runs a tool-use loop: it receives a task description, calls tools
//! (read_file, write_file, run_command, list_dir, search_files) until it's
//! satisfied, then returns. The loop stops when the model reaches "end_turn"
//! or the max-turn limit is hit.

use anyhow::{Context, Result};
use colored::Colorize;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

use crate::llm::anthropic::apply_auth_headers;

const ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const MAX_FILE_READ_BYTES: usize = 128 * 1024; // 128 KB

// ─── Tool definitions ────────────────────────────────────────────────────────

fn tool_definitions() -> Value {
    json!([
        {
            "name": "read_file",
            "description": "Read the contents of a file. Returns the file content as text.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path (relative to working directory or absolute)"}
                },
                "required": ["path"]
            }
        },
        {
            "name": "write_file",
            "description": "Write content to a file, creating parent directories as needed. Overwrites if the file exists.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path"},
                    "content": {"type": "string", "description": "Full content to write"}
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "edit_file",
            "description": "Replace an exact string in a file with new text. Fails if the old_str is not found or matches more than once. Prefer this over write_file for targeted edits.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "File path"},
                    "old_str": {"type": "string", "description": "Exact string to find (must match exactly, including whitespace)"},
                    "new_str": {"type": "string", "description": "Replacement string"}
                },
                "required": ["path", "old_str", "new_str"]
            }
        },
        {
            "name": "run_command",
            "description": "Run a shell command in the working directory. Returns stdout and stderr. Use for cargo, git, find, grep, ls, and anything else.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command to run (via sh -c)"},
                    "timeout_seconds": {"type": "integer", "description": "Timeout in seconds (default 120)"}
                },
                "required": ["command"]
            }
        }
    ])
}

// ─── API types ───────────────────────────────────────────────────────────────

#[derive(Serialize, Clone)]
#[serde(tag = "role")]
enum Message {
    #[serde(rename = "user")]
    User { content: Vec<ContentBlock> },
    #[serde(rename = "assistant")]
    Assistant { content: Vec<ContentBlock> },
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(tag = "type")]
enum ContentBlock {
    #[serde(rename = "text")]
    Text { text: String },
    #[serde(rename = "tool_use")]
    ToolUse {
        id: String,
        name: String,
        input: Value,
    },
    #[serde(rename = "tool_result")]
    ToolResult {
        tool_use_id: String,
        content: String,
    },
}

#[derive(Deserialize, Debug, Default)]
struct Usage {
    input_tokens: u32,
    #[allow(dead_code)]
    output_tokens: u32,
}

#[derive(Deserialize, Debug)]
struct ApiResponse {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
    #[serde(default)]
    usage: Usage,
}

// ─── Tool execution ───────────────────────────────────────────────────────────

fn resolve_path(workdir: &Path, path: &str) -> PathBuf {
    let p = Path::new(path);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        workdir.join(p)
    }
}

fn execute_read_file(workdir: &Path, input: &Value) -> String {
    let path_str = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'path'".to_string(),
    };
    let path = resolve_path(workdir, path_str);
    match std::fs::read(&path) {
        Ok(bytes) => {
            if bytes.len() > MAX_FILE_READ_BYTES {
                let truncated = &bytes[..MAX_FILE_READ_BYTES];
                let text = String::from_utf8_lossy(truncated);
                format!(
                    "{}\n\n[...truncated: file is {} bytes, showing first {}]",
                    text,
                    bytes.len(),
                    MAX_FILE_READ_BYTES
                )
            } else {
                String::from_utf8_lossy(&bytes).to_string()
            }
        }
        Err(e) => format!("Error reading {path_str}: {e}"),
    }
}

fn execute_write_file(workdir: &Path, input: &Value) -> String {
    let path_str = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'path'".to_string(),
    };
    let content = match input.get("content").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return "Error: missing required parameter 'content'".to_string(),
    };
    let path = resolve_path(workdir, path_str);
    if let Some(parent) = path.parent() {
        if let Err(e) = std::fs::create_dir_all(parent) {
            return format!("Error creating directories for {path_str}: {e}");
        }
    }
    match std::fs::write(&path, content) {
        Ok(()) => format!("Written {} bytes to {path_str}", content.len()),
        Err(e) => format!("Error writing {path_str}: {e}"),
    }
}

fn execute_edit_file(workdir: &Path, input: &Value) -> String {
    let path_str = match input.get("path").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'path'".to_string(),
    };
    let old_str = match input.get("old_str").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return "Error: missing required parameter 'old_str'".to_string(),
    };
    let new_str = match input.get("new_str").and_then(|v| v.as_str()) {
        Some(s) => s,
        None => return "Error: missing required parameter 'new_str'".to_string(),
    };
    let file_path = resolve_path(workdir, path_str);
    let content = match std::fs::read_to_string(&file_path) {
        Ok(c) => c,
        Err(e) => return format!("Error reading {path_str}: {e}"),
    };
    let count = content.matches(old_str).count();
    match count {
        0 => format!("Error: old_str not found in {path_str}"),
        1 => {
            let updated = content.replacen(old_str, new_str, 1);
            match std::fs::write(&file_path, &updated) {
                Ok(()) => format!(
                    "Edited {path_str}: replaced {} chars with {} chars",
                    old_str.len(),
                    new_str.len()
                ),
                Err(e) => format!("Error writing {path_str}: {e}"),
            }
        }
        n => format!("Error: old_str matches {n} times in {path_str} — be more specific"),
    }
}

async fn execute_run_command(workdir: &Path, input: &Value) -> String {
    let command = match input.get("command").and_then(|v| v.as_str()) {
        Some(c) => c,
        None => return "Error: missing required parameter 'command'".to_string(),
    };
    let timeout_secs = input
        .get("timeout_seconds")
        .and_then(|v| v.as_u64())
        .unwrap_or(120);

    let result = tokio::time::timeout(
        std::time::Duration::from_secs(timeout_secs),
        tokio::process::Command::new("sh")
            .arg("-c")
            .arg(command)
            .current_dir(workdir)
            .output(),
    )
    .await;

    match result {
        Err(_) => format!("Command timed out after {timeout_secs}s: {command}"),
        Ok(Err(e)) => format!("Failed to spawn command: {e}"),
        Ok(Ok(output)) => {
            let stdout = String::from_utf8_lossy(&output.stdout);
            let stderr = String::from_utf8_lossy(&output.stderr);
            let exit = output.status.code().unwrap_or(-1);
            let mut result = String::new();
            if !stdout.is_empty() {
                result.push_str(&stdout);
            }
            if !stderr.is_empty() {
                if !result.is_empty() {
                    result.push('\n');
                }
                result.push_str(&stderr);
            }
            if result.is_empty() {
                result = format!("(exit {})", exit);
            } else {
                result.push_str(&format!("\n[exit {}]", exit));
            }
            result
        }
    }
}

// ─── Main agent ──────────────────────────────────────────────────────────────

pub struct ApiCodingAgent {
    api_key: String,
    model: String,
    max_turns: u32,
    http: reqwest::Client,
}

impl ApiCodingAgent {
    pub fn new(api_key: String, model: String, max_turns: u32) -> Self {
        ApiCodingAgent {
            api_key,
            model,
            max_turns,
            http: reqwest::Client::new(),
        }
    }

    /// Run the agent loop: send prompt, execute tools, loop until done.
    pub async fn run(&self, system: &str, prompt: &str, workdir: &Path) -> Result<()> {
        let mut messages: Vec<Message> = vec![Message::User {
            content: vec![ContentBlock::Text {
                text: prompt.to_string(),
            }],
        }];

        let tools = tool_definitions();

        for turn in 0..self.max_turns {
            println!(
                "{}",
                format!("  [api-agent turn {}/{}]", turn + 1, self.max_turns).dimmed()
            );

            let request = json!({
                "model": self.model,
                "max_tokens": 8192,
                "system": system,
                "tools": tools,
                "messages": messages
            });

            let response = self.call_api(&request).await?;

            // Show token usage
            let input_tokens = response.usage.input_tokens;
            println!(
                "{}",
                format!("  [tokens: {}K in]", input_tokens / 1000).dimmed()
            );

            // Compact context if approaching the limit (threshold: 150K input tokens).
            // Keep messages[0] (original task) + last 4 messages; summarize everything in between.
            if input_tokens > 150_000 && messages.len() > 6 {
                println!("{}", "  [context >150K tokens — compacting...]".yellow());
                messages = self.compact_context(system, &messages).await;
                println!("{}", "  [compaction done]".dimmed());
            }

            // Print any text blocks
            for block in &response.content {
                if let ContentBlock::Text { text } = block {
                    if !text.trim().is_empty() {
                        println!("{}", text);
                    }
                }
            }

            // Collect tool calls
            let tool_calls: Vec<&ContentBlock> = response
                .content
                .iter()
                .filter(|b| matches!(b, ContentBlock::ToolUse { .. }))
                .collect();

            if tool_calls.is_empty() {
                // No tool calls — agent is done
                break;
            }

            // Push assistant message
            messages.push(Message::Assistant {
                content: response.content.clone(),
            });

            // Execute tools and build tool_result blocks
            let mut result_blocks: Vec<ContentBlock> = Vec::new();
            for block in &tool_calls {
                if let ContentBlock::ToolUse { id, name, input } = block {
                    println!(
                        "{}",
                        format!("  ⚙ {}({})", name, summarize_input(input)).cyan()
                    );
                    let result = execute_tool(name, input, workdir).await;
                    println!("  {} {}", "→".dimmed(), first_line(&result).dimmed());
                    result_blocks.push(ContentBlock::ToolResult {
                        tool_use_id: id.clone(),
                        content: result,
                    });
                }
            }

            messages.push(Message::User {
                content: result_blocks,
            });

            if response.stop_reason.as_deref() == Some("end_turn") {
                break;
            }
        }

        Ok(())
    }

    // Compact conversation context by summarising middle turns.
    // Keeps messages[0] (original task prompt) + last 4 messages verbatim.
    // Everything in between is sent to the LLM for summarisation.
    async fn compact_context(&self, system: &str, messages: &[Message]) -> Vec<Message> {
        if messages.len() <= 6 {
            return messages.to_vec();
        }

        // Tail: last 4 messages kept verbatim (2 turns = assistant + tool_results each)
        let tail_start = messages.len() - 4;
        let head = &messages[0..1]; // original task
        let middle = &messages[1..tail_start];
        let tail = &messages[tail_start..];

        // Serialize middle turns to text for the summary prompt
        let mut history_text = String::new();
        for msg in middle {
            match msg {
                Message::Assistant { content } => {
                    for block in content {
                        match block {
                            ContentBlock::Text { text } => {
                                history_text.push_str(&format!(
                                    "ASSISTANT: {text}
"
                                ));
                            }
                            ContentBlock::ToolUse { name, input, .. } => {
                                history_text.push_str(&format!(
                                    "TOOL CALL: {name}({})
",
                                    summarize_input(input)
                                ));
                            }
                            _ => {}
                        }
                    }
                }
                Message::User { content } => {
                    for block in content {
                        if let ContentBlock::ToolResult { content, .. } = block {
                            let preview = if content.len() > 300 {
                                format!("{}...[truncated]", &content[..300])
                            } else {
                                content.clone()
                            };
                            history_text.push_str(&format!(
                                "TOOL RESULT: {preview}
"
                            ));
                        }
                    }
                }
            }
        }

        let summary_prompt = format!(
            "You are summarizing the progress of a coding agent session.
             The agent is implementing a software task. Below is the conversation history              (tool calls and results) from the middle of the session.

             Create a concise summary (500-800 words) that captures:
             - What files were read and their key contents
             - What changes were made and to which files
             - What commands were run and their outcomes
             - Current state: what is done, what still needs doing

             Be specific about file paths and code details.

             HISTORY:
{history_text}"
        );

        let summary_req = json!({
            "model": self.model,
            "max_tokens": 1024,
            "system": system,
            "messages": [{"role": "user", "content": summary_prompt}]
        });

        let summary_text = match self.call_api(&summary_req).await {
            Ok(resp) => resp
                .content
                .into_iter()
                .filter_map(|b| {
                    if let ContentBlock::Text { text } = b {
                        Some(text)
                    } else {
                        None
                    }
                })
                .collect::<Vec<_>>()
                .join("\n"),
            Err(e) => {
                eprintln!("  [compaction failed: {e} — keeping full context]");
                return messages.to_vec();
            }
        };

        // Rebuild: [original_task, summary, ...tail]
        let mut compacted = head.to_vec();
        compacted.push(Message::User {
            content: vec![ContentBlock::Text {
                text: format!(
                    "[CONTEXT SUMMARY — earlier turns compacted to save context]

{summary_text}"
                ),
            }],
        });
        compacted.extend_from_slice(tail);
        compacted
    }

    async fn call_api(&self, body: &Value) -> Result<ApiResponse> {
        let req = self
            .http
            .post(ANTHROPIC_API_URL)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        let req = apply_auth_headers(req, &self.api_key);

        let resp = req
            .json(body)
            .send()
            .await
            .context("Failed to reach Anthropic API")?;

        let status = resp.status();
        if !status.is_success() {
            let body_text = resp
                .text()
                .await
                .unwrap_or_else(|_| "(unreadable)".to_string());
            // Extract message from JSON error if possible
            let msg = serde_json::from_str::<Value>(&body_text)
                .ok()
                .and_then(|v| v.get("error")?.get("message")?.as_str().map(String::from))
                .unwrap_or(body_text);
            anyhow::bail!("Anthropic API error ({}): {}", status, msg);
        }

        resp.json::<ApiResponse>()
            .await
            .context("Failed to parse Anthropic API response")
    }
}

async fn execute_tool(name: &str, input: &Value, workdir: &Path) -> String {
    match name {
        "read_file" => execute_read_file(workdir, input),
        "write_file" => execute_write_file(workdir, input),
        "edit_file" => execute_edit_file(workdir, input),
        "run_command" => execute_run_command(workdir, input).await,
        other => format!("Unknown tool: {other}"),
    }
}

fn summarize_input(input: &Value) -> String {
    // Show the first string value in the input for a compact log line
    if let Value::Object(map) = input {
        for val in map.values() {
            if let Value::String(s) = val {
                let truncated = if s.len() > 60 { &s[..60] } else { s };
                return format!("\"{}\"", truncated.replace('\n', "↵"));
            }
        }
    }
    String::new()
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

// ─── Tests ───────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_resolve_path_relative() {
        let workdir = Path::new("/tmp/project");
        let result = resolve_path(workdir, "src/main.rs");
        assert_eq!(result, PathBuf::from("/tmp/project/src/main.rs"));
    }

    #[test]
    fn test_resolve_path_absolute() {
        let workdir = Path::new("/tmp/project");
        let result = resolve_path(workdir, "/etc/hosts");
        assert_eq!(result, PathBuf::from("/etc/hosts"));
    }

    #[test]
    fn test_execute_read_file_ok() {
        let tmp = TempDir::new().unwrap();
        let file_path = tmp.path().join("hello.txt");
        std::fs::write(&file_path, "hello world").unwrap();
        let input = json!({"path": "hello.txt"});
        let result = execute_read_file(tmp.path(), &input);
        assert_eq!(result, "hello world");
    }

    #[test]
    fn test_execute_read_file_missing() {
        let tmp = TempDir::new().unwrap();
        let input = json!({"path": "nope.txt"});
        let result = execute_read_file(tmp.path(), &input);
        assert!(result.starts_with("Error reading"));
    }

    #[test]
    fn test_execute_write_file_ok() {
        let tmp = TempDir::new().unwrap();
        let input = json!({"path": "out.txt", "content": "hello"});
        let result = execute_write_file(tmp.path(), &input);
        assert!(result.contains("Written"));
        assert_eq!(
            std::fs::read_to_string(tmp.path().join("out.txt")).unwrap(),
            "hello"
        );
    }

    #[test]
    fn test_execute_write_file_creates_dirs() {
        let tmp = TempDir::new().unwrap();
        let input = json!({"path": "a/b/c.txt", "content": "deep"});
        let result = execute_write_file(tmp.path(), &input);
        assert!(result.contains("Written"));
        assert!(tmp.path().join("a/b/c.txt").exists());
    }

    #[test]
    fn test_execute_edit_file_ok() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.rs"), "fn foo() {}\nfn bar() {}").unwrap();
        let input =
            json!({"path": "f.rs", "old_str": "fn foo() {}", "new_str": "fn foo() { todo!() }"});
        let result = execute_edit_file(tmp.path(), &input);
        assert!(result.contains("Edited"), "got: {result}");
        let content = std::fs::read_to_string(tmp.path().join("f.rs")).unwrap();
        assert!(content.contains("fn foo() { todo!() }"));
        assert!(content.contains("fn bar() {}"));
    }

    #[test]
    fn test_execute_edit_file_not_found() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.rs"), "fn foo() {}").unwrap();
        let input = json!({"path": "f.rs", "old_str": "fn missing() {}", "new_str": "x"});
        let result = execute_edit_file(tmp.path(), &input);
        assert!(result.contains("not found"), "got: {result}");
    }

    #[test]
    fn test_execute_edit_file_multiple_matches() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("f.rs"), "x\nx\nx").unwrap();
        let input = json!({"path": "f.rs", "old_str": "x", "new_str": "y"});
        let result = execute_edit_file(tmp.path(), &input);
        assert!(result.contains("matches 3 times"), "got: {result}");
    }

    #[test]
    fn test_missing_path_param() {
        let tmp = TempDir::new().unwrap();
        let result = execute_read_file(tmp.path(), &json!({}));
        assert!(result.contains("Error: missing required parameter"));
    }

    #[test]
    fn test_first_line() {
        assert_eq!(first_line("hello\nworld"), "hello");
        assert_eq!(first_line("single"), "single");
    }

    #[test]
    fn test_summarize_input() {
        let input = json!({"path": "src/main.rs"});
        assert!(summarize_input(&input).contains("src/main.rs"));
    }

    #[tokio::test]
    async fn test_run_command_exit_code() {
        let tmp = TempDir::new().unwrap();
        let input = json!({"command": "echo hello"});
        let result = execute_run_command(tmp.path(), &input).await;
        assert!(result.contains("hello"));
    }

    #[tokio::test]
    async fn test_run_command_failure() {
        let tmp = TempDir::new().unwrap();
        let input = json!({"command": "false"});
        let result = execute_run_command(tmp.path(), &input).await;
        // no output → compact form; with output → appended form
        assert!(result.contains("exit 1"), "got: {result}");
    }

    #[tokio::test]
    async fn test_run_command_timeout() {
        let tmp = TempDir::new().unwrap();
        let input = json!({"command": "sleep 10", "timeout_seconds": 1});
        let result = execute_run_command(tmp.path(), &input).await;
        assert!(result.contains("timed out"));
    }
}
