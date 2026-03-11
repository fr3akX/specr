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
    pub fn build_prompt(task: &Task, spec_content: &str, task_detail: &str) -> String {
        format!(
            "You are implementing task {id}: {name}\n\n\
             SPEC.md:\n{spec}\n\n\
             Task details:\n{detail}\n\n\
             Implement exactly what is described. Stay within the scope defined in \"What NOT to change\".\n\
             When done, run: cargo test && cargo clippy",
            id = task.id,
            name = task.name,
            spec = spec_content,
            detail = task_detail,
        )
    }

    /// Build the prompt for a retry with review findings.
    pub fn build_retry_prompt(
        task: &Task,
        spec_content: &str,
        task_detail: &str,
        findings: &str,
    ) -> String {
        format!(
            "You are implementing task {id}: {name}\n\n\
             SPEC.md:\n{spec}\n\n\
             Task details:\n{detail}\n\n\
             The previous implementation was reviewed and had issues:\n\n{findings}\n\n\
             Fix all critical issues listed above. Stay within the scope defined in \"What NOT to change\".\n\
             When done, run: cargo test && cargo clippy",
            id = task.id,
            name = task.name,
            spec = spec_content,
            detail = task_detail,
            findings = findings,
        )
    }

    /// Spawn Claude Code with the given prompt.
    /// Streams stdout/stderr to terminal in real time.
    pub async fn run(
        &self,
        task: &Task,
        spec_content: &str,
        task_detail: &str,
        workdir: &Path,
        retry_findings: Option<&str>,
    ) -> Result<()> {
        let prompt = match retry_findings {
            Some(findings) => {
                Self::build_retry_prompt(task, spec_content, task_detail, findings)
            }
            None => Self::build_prompt(task, spec_content, task_detail),
        };

        self.spawn_with_prompt(&prompt, workdir).await
    }

    /// Spawn the CLI binary with a prompt, streaming output.
    async fn spawn_with_prompt(&self, prompt: &str, workdir: &Path) -> Result<()> {
        let mut child = Command::new(&self.bin)
            .arg("--print")
            .arg(prompt)
            .current_dir(workdir)
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .with_context(|| format!("Failed to spawn {}", self.bin))?;

        // Stream stdout
        if let Some(stdout) = child.stdout.take() {
            let reader = BufReader::new(stdout);
            let mut lines = reader.lines();
            while let Some(line) = lines.next_line().await? {
                println!("{}", line);
            }
        }

        // Stream stderr
        if let Some(stderr) = child.stderr.take() {
            let reader = BufReader::new(stderr);
            let mut lines = reader.lines();
            while let Some(line) = lines.next_line().await? {
                eprintln!("{}", line);
            }
        }

        let status = child.wait().await?;
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
        let prompt = CodingAgent::build_prompt(&task, "spec content", "task detail");
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
        );
        assert!(prompt.contains("task 001: Scaffold project"));
        assert!(prompt.contains("previous implementation was reviewed"));
        assert!(prompt.contains("Critical: missing error handling"));
        assert!(prompt.contains("Fix all critical issues"));
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
