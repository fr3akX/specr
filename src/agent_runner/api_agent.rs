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
                    "path": {"type": "string", "description": "Path to the file (relative to working directory or absolute)"}
                },
                "required": ["path"]
            }
        },
        {
            "name": "write_file",
            "description": "Write content to a file, creating parent directories as needed. Overwrites if the file already exists.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Path to the file"},
                    "content": {"type": "string", "description": "Content to write"}
                },
                "required": ["path", "content"]
            }
        },
        {
            "name": "run_command",
            "description": "Run a shell command in the working directory. Returns stdout and stderr. Use for cargo build/test/clippy, git operations, etc.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "command": {"type": "string", "description": "Shell command to run"},
                    "timeout_seconds": {"type": "integer", "description": "Timeout in seconds (default 120)"}
                },
                "required": ["command"]
            }
        },
        {
            "name": "list_dir",
            "description": "List the contents of a directory.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "path": {"type": "string", "description": "Directory path (default: working directory)"}
                },
                "required": []
            }
        },
        {
            "name": "search_files",
            "description": "Search for a text pattern across files in a directory. Returns matching lines with file paths.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "pattern": {"type": "string", "description": "Text to search for"},
                    "directory": {"type": "string", "description": "Directory to search in (default: working directory)"},
                    "file_pattern": {"type": "string", "description": "Glob-like file extension filter, e.g. '*.rs' (optional)"}
                },
                "required": ["pattern"]
            }
        },
        {
            "name": "find_files",
            "description": "Find files matching a name pattern in a directory tree.",
            "input_schema": {
                "type": "object",
                "properties": {
                    "name_pattern": {"type": "string", "description": "Filename pattern (substring match)"},
                    "directory": {"type": "string", "description": "Root directory to search (default: working directory)"}
                },
                "required": ["name_pattern"]
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

#[derive(Deserialize, Debug)]
struct ApiResponse {
    content: Vec<ContentBlock>,
    stop_reason: Option<String>,
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

fn execute_list_dir(workdir: &Path, input: &Value) -> String {
    let path_str = input.get("path").and_then(|v| v.as_str()).unwrap_or(".");
    let path = resolve_path(workdir, path_str);
    match std::fs::read_dir(&path) {
        Err(e) => format!("Error reading directory {path_str}: {e}"),
        Ok(entries) => {
            let mut items: Vec<String> = entries
                .filter_map(|e| e.ok())
                .map(|e| {
                    let name = e.file_name().to_string_lossy().to_string();
                    if e.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                        format!("{}/", name)
                    } else {
                        name
                    }
                })
                .collect();
            items.sort();
            if items.is_empty() {
                format!("{path_str}: (empty)")
            } else {
                format!("{path_str}:\n{}", items.join("\n"))
            }
        }
    }
}

fn execute_search_files(workdir: &Path, input: &Value) -> String {
    let pattern = match input.get("pattern").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'pattern'".to_string(),
    };
    let dir_str = input
        .get("directory")
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    let file_ext = input
        .get("file_pattern")
        .and_then(|v| v.as_str())
        .and_then(|p| p.strip_prefix("*."))
        .map(|s| s.to_string());

    let search_dir = resolve_path(workdir, dir_str);
    let mut matches: Vec<String> = Vec::new();
    search_recursive(
        &search_dir,
        &search_dir,
        pattern,
        file_ext.as_deref(),
        &mut matches,
        0,
    );

    if matches.is_empty() {
        format!("No matches for '{}' in {}", pattern, dir_str)
    } else {
        let truncated = matches.len() > 100;
        let shown: Vec<_> = matches.into_iter().take(100).collect();
        let mut out = shown.join("\n");
        if truncated {
            out.push_str("\n[...more matches not shown]");
        }
        out
    }
}

fn search_recursive(
    root: &Path,
    dir: &Path,
    pattern: &str,
    ext_filter: Option<&str>,
    matches: &mut Vec<String>,
    depth: u32,
) {
    if depth > 8 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();

        // Skip hidden dirs and common noise
        if name_str.starts_with('.') || name_str == "target" || name_str == "node_modules" {
            continue;
        }

        if path.is_dir() {
            search_recursive(root, &path, pattern, ext_filter, matches, depth + 1);
        } else if path.is_file() {
            if let Some(ext) = ext_filter {
                if !name_str.ends_with(&format!(".{}", ext)) {
                    continue;
                }
            }
            if let Ok(content) = std::fs::read_to_string(&path) {
                let rel = path.strip_prefix(root).unwrap_or(&path);
                for (i, line) in content.lines().enumerate() {
                    if line.contains(pattern) {
                        matches.push(format!("{}:{}: {}", rel.display(), i + 1, line.trim()));
                        if matches.len() >= 200 {
                            return;
                        }
                    }
                }
            }
        }
    }
}

fn execute_find_files(workdir: &Path, input: &Value) -> String {
    let pattern = match input.get("name_pattern").and_then(|v| v.as_str()) {
        Some(p) => p,
        None => return "Error: missing required parameter 'name_pattern'".to_string(),
    };
    let dir_str = input
        .get("directory")
        .and_then(|v| v.as_str())
        .unwrap_or(".");
    let search_dir = resolve_path(workdir, dir_str);

    let mut found: Vec<String> = Vec::new();
    find_recursive(&search_dir, &search_dir, pattern, &mut found, 0);

    if found.is_empty() {
        format!("No files matching '{}' found", pattern)
    } else {
        found.join("\n")
    }
}

fn find_recursive(root: &Path, dir: &Path, pattern: &str, found: &mut Vec<String>, depth: u32) {
    if depth > 8 || found.len() >= 100 {
        return;
    }
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(_) => return,
    };
    for entry in entries.filter_map(|e| e.ok()) {
        let path = entry.path();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with('.') || name_str == "target" || name_str == "node_modules" {
            continue;
        }
        if path.is_dir() {
            find_recursive(root, &path, pattern, found, depth + 1);
        } else if name_str.contains(pattern) {
            let rel = path.strip_prefix(root).unwrap_or(&path);
            found.push(rel.display().to_string());
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

    async fn call_api(&self, body: &Value) -> Result<ApiResponse> {
        let mut req = self
            .http
            .post(ANTHROPIC_API_URL)
            .header("anthropic-version", "2023-06-01")
            .header("content-type", "application/json");

        if self.api_key.starts_with("sk-ant-oat") {
            req = req.header("Authorization", format!("Bearer {}", self.api_key));
        } else {
            req = req.header("x-api-key", &self.api_key);
        }

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
        "run_command" => execute_run_command(workdir, input).await,
        "list_dir" => execute_list_dir(workdir, input),
        "search_files" => execute_search_files(workdir, input),
        "find_files" => execute_find_files(workdir, input),
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
    fn test_execute_list_dir() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("foo.rs"), "").unwrap();
        std::fs::create_dir(tmp.path().join("bar")).unwrap();
        let input = json!({});
        let result = execute_list_dir(tmp.path(), &input);
        assert!(result.contains("bar/"));
        assert!(result.contains("foo.rs"));
    }

    #[test]
    fn test_execute_search_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("a.rs"), "fn main() {}\nfn helper() {}").unwrap();
        std::fs::write(tmp.path().join("b.rs"), "fn test() {}").unwrap();
        let input = json!({"pattern": "fn main"});
        let result = execute_search_files(tmp.path(), &input);
        assert!(result.contains("fn main"));
        assert!(result.contains("a.rs"));
    }

    #[test]
    fn test_execute_find_files() {
        let tmp = TempDir::new().unwrap();
        std::fs::write(tmp.path().join("config.toml"), "").unwrap();
        std::fs::write(tmp.path().join("main.rs"), "").unwrap();
        let input = json!({"name_pattern": ".toml"});
        let result = execute_find_files(tmp.path(), &input);
        assert!(result.contains("config.toml"));
        assert!(!result.contains("main.rs"));
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
