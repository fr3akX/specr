use anyhow::{Context, Result};
use std::path::Path;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::process::Command;

use crate::types::Task;

/// Spawns Claude Code CLI as a subprocess to implement a task.
pub struct CodingAgent {
    bin: String,
}

impl CodingAgent {
    pub fn new(bin: &str) -> Self {
        CodingAgent {
            bin: bin.to_string(),
        }
    }

    /// Build the prompt for the coding agent.
    /// `extra_instructions` is appended if non-empty (from .specr/coder.md + task instructions).
    pub fn build_prompt(
        task: &Task,
        spec_content: &str,
        task_detail: &str,
        extra_instructions: &str,
    ) -> String {
        let commit_msg = format!("task {}: {}", task.id, task.name);
        let instructions_block = crate::instructions::as_system_appendix(extra_instructions);
        format!(
            "You are implementing task {id}: {name}\n\n\
             SPEC.md:\n{spec}\n\n\
             Task details:\n{detail}\n\n\
             Implement exactly what is described. Stay within the scope defined in \"What NOT to change\".\
             {instructions}\n\
             When done:\n\
             1. Run: cargo test && cargo clippy -- -D warnings\n\
             2. Stage modified files and commit: git add -u && git add <any-new-files> && git commit -m \"{commit}\"",
            id = task.id,
            name = task.name,
            spec = spec_content,
            detail = task_detail,
            instructions = instructions_block,
            commit = commit_msg,
        )
    }

    /// Build the prompt for a retry with review findings.
    /// `already_done` is a summary of what has been committed so far (git log/diff --stat).
    #[allow(clippy::too_many_arguments)]
    pub fn build_retry_prompt(
        task: &Task,
        spec_content: &str,
        task_detail: &str,
        findings: &str,
        already_done: &str,
        extra_instructions: &str,
    ) -> String {
        let done_section = if already_done.trim().is_empty() {
            String::new()
        } else {
            format!(
                "## Already committed on this branch\n\
                 The following work was done in previous iterations — DO NOT redo it:\n\n\
                 {already_done}\n\n"
            )
        };
        let instructions_block = crate::instructions::as_system_appendix(extra_instructions);
        format!(
            "You are implementing task {id}: {name}\n\n\
             SPEC.md:\n{spec}\n\n\
             Task details:\n{detail}\n\n\
             {done}\
             ## Review findings — fix only these remaining issues\n\n\
             {findings}\n\n\
             Focus on what is still missing. Build on existing work — do not rewrite what is already committed.\
             {instructions}\n\
             When done:\n\
             1. Run: cargo test && cargo clippy -- -D warnings\n\
             2. Stage modified files and commit: git add -u && git add <any-new-files> && git commit -m \"task {id}: {name} (retry)\"",
            id = task.id,
            name = task.name,
            spec = spec_content,
            detail = task_detail,
            done = done_section,
            findings = findings,
            instructions = instructions_block,
        )
    }

    /// Spawn Claude Code with the given prompt.
    /// Streams stdout/stderr to terminal in real time.
    #[allow(clippy::too_many_arguments)]
    pub async fn run(
        &self,
        task: &Task,
        spec_content: &str,
        task_detail: &str,
        workdir: &Path,
        retry_findings: Option<&str>,
        already_done: &str,
        extra_instructions: &str,
    ) -> Result<()> {
        let prompt = match retry_findings {
            Some(findings) => Self::build_retry_prompt(
                task,
                spec_content,
                task_detail,
                findings,
                already_done,
                extra_instructions,
            ),
            None => Self::build_prompt(task, spec_content, task_detail, extra_instructions),
        };

        self.spawn_with_prompt(&prompt, workdir).await
    }

    /// Spawn the CLI binary with a prompt, streaming stdout and stderr concurrently.
    async fn spawn_with_prompt(&self, prompt: &str, workdir: &Path) -> Result<()> {
        let mut child = Command::new(&self.bin)
            .arg("-p")
            .arg("--dangerously-skip-permissions")
            .arg("--no-session-persistence")
            .arg(prompt)
            .current_dir(workdir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn {}", self.bin))?;

        // Stream stdout and stderr concurrently to avoid deadlock when both
        // buffers fill up. Reading them sequentially can cause the subprocess
        // to block writing to the full buffer while we're still draining the other.
        let stdout_task = if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            tokio::spawn(async move {
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    println!("{}", line);
                }
            })
        } else {
            tokio::spawn(async {})
        };

        let stderr_task = if let Some(stderr) = child.stderr.take() {
            let reader = BufReader::new(stderr);
            tokio::spawn(async move {
                let mut lines = reader.lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    eprintln!("{}", line);
                }
            })
        } else {
            tokio::spawn(async {})
        };

        let status = child.wait().await?;
        // Wait for both stream tasks to flush before returning
        let _ = tokio::join!(stdout_task, stderr_task);

        if !status.success() {
            anyhow::bail!(
                "{} exited with status {}",
                self.bin,
                status.code().unwrap_or(-1)
            );
        }

        Ok(())
    }
}

/// Build a git command in the given workdir.
pub async fn git_command(workdir: &Path, args: &[&str]) -> Result<String> {
    let output = Command::new("git")
        .args(args)
        .current_dir(workdir)
        .output()
        .await
        .with_context(|| format!("Failed to run git {}", args.join(" ")))?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        anyhow::bail!("git {} failed: {}", args.join(" "), stderr.trim());
    }

    Ok(String::from_utf8_lossy(&output.stdout).trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{TaskSize, TaskStatus};

    fn make_task() -> Task {
        Task {
            id: "001".to_string(),
            name: "Scaffold project".to_string(),
            size: TaskSize::S,
            status: TaskStatus::Open,
            depends_on: vec![],
            done_when: "cargo build passes".to_string(),
            scope: "Create project structure".to_string(),
            files_to_touch: vec!["Cargo.toml".to_string()],
            not_to_change: vec![],
            branch: "task/001-scaffold-project".to_string(),
            interface: None,
        }
    }

    #[test]
    fn test_build_prompt() {
        let task = make_task();
        let prompt = CodingAgent::build_prompt(&task, "spec content", "task detail", "");
        assert!(prompt.contains("task 001: Scaffold project"));
        assert!(prompt.contains("spec content"));
        assert!(prompt.contains("task detail"));
        assert!(prompt.contains("cargo test && cargo clippy"));
    }

    #[test]
    fn test_build_retry_prompt() {
        let task = make_task();
        let prompt = CodingAgent::build_retry_prompt(
            &task,
            "spec content",
            "task detail",
            "Critical: missing error handling",
            "Commits:\nabc123 add tests",
            "",
        );
        assert!(prompt.contains("task 001: Scaffold project"));
        assert!(prompt.contains("Review findings"));
        assert!(prompt.contains("Critical: missing error handling"));
        assert!(prompt.contains("Already committed"));
    }

    #[test]
    fn test_coding_agent_new() {
        let agent = CodingAgent::new("claude");
        assert_eq!(agent.bin, "claude");
    }

    #[test]
    fn test_coding_agent_custom_bin() {
        let agent = CodingAgent::new("/usr/local/bin/my-agent");
        assert_eq!(agent.bin, "/usr/local/bin/my-agent");
    }
}
