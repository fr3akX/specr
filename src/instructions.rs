/// Agent instruction loading.
///
/// Instructions are markdown files that get appended to agent system prompts,
/// letting you add project-specific coding rules, review criteria, etc.
///
/// Directory structure:
///
/// Global (applies to all projects):
///   ~/.config/specr/instructions/
///     coder.md
///     code-reviewer.md
///     qa-reviewer.md
///     style-reviewer.md
///     coordinator.md
///
/// Project-level (applies only to this project):
///   <workdir>/.specr/
///     coder.md
///     code-reviewer.md
///     qa-reviewer.md
///     style-reviewer.md
///     coordinator.md
///
/// Per-task (applies only to the current task's agent run):
///   In the task detail file, a section headed `## Agent Instructions`.
///   Everything under that section until the next `##` is task-scoped.
///
/// Combining: global + project are concatenated (global first, project
/// appended). Per-task is appended on top of that. Empty = no extra context.
use std::path::{Path, PathBuf};

use anyhow::Result;

use crate::config::Config;
use crate::llm::LlmClient;

// ── Agent kinds ───────────────────────────────────────────────────────────────

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentKind {
    Coder,
    CodeReviewer,
    QaReviewer,
    StyleReviewer,
    Coordinator,
}

impl AgentKind {
    pub fn all() -> &'static [AgentKind] {
        &[
            AgentKind::Coder,
            AgentKind::CodeReviewer,
            AgentKind::QaReviewer,
            AgentKind::StyleReviewer,
            AgentKind::Coordinator,
        ]
    }

    pub fn filename(self) -> &'static str {
        match self {
            AgentKind::Coder => "coder.md",
            AgentKind::CodeReviewer => "code-reviewer.md",
            AgentKind::QaReviewer => "qa-reviewer.md",
            AgentKind::StyleReviewer => "style-reviewer.md",
            AgentKind::Coordinator => "coordinator.md",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            AgentKind::Coder => "Coder",
            AgentKind::CodeReviewer => "Code Reviewer",
            AgentKind::QaReviewer => "QA Reviewer",
            AgentKind::StyleReviewer => "Style Reviewer",
            AgentKind::Coordinator => "Coordinator",
        }
    }
}

// ── Path helpers ──────────────────────────────────────────────────────────────

/// `~/.config/specr/instructions/`
fn global_dir() -> Option<PathBuf> {
    dirs::config_dir().map(|d| d.join("specr").join("instructions"))
}

/// `<workdir>/.specr/`
fn project_dir(workdir: &Path) -> PathBuf {
    workdir.join(".specr")
}

fn read_file_opt(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

// ── Loading ───────────────────────────────────────────────────────────────────

/// Load and combine global + project instructions for `kind`.
/// Returns an empty string if neither file exists or both are empty.
pub fn load(workdir: &Path, kind: AgentKind) -> String {
    let global = global_dir()
        .and_then(|d| read_file_opt(&d.join(kind.filename())))
        .unwrap_or_default();

    let project = read_file_opt(&project_dir(workdir).join(kind.filename())).unwrap_or_default();

    combine(&global, &project)
}

/// Load global + project instructions AND extract per-task instructions from
/// the task detail markdown (the `## Agent Instructions` section, if present).
pub fn load_with_task(workdir: &Path, kind: AgentKind, task_detail: &str) -> String {
    let base = load(workdir, kind);
    let task_instructions = extract_task_instructions(task_detail);
    combine(&base, &task_instructions)
}

fn combine(a: &str, b: &str) -> String {
    match (a.is_empty(), b.is_empty()) {
        (true, true) => String::new(),
        (false, true) => a.to_string(),
        (true, false) => b.to_string(),
        (false, false) => format!("{a}\n\n{b}"),
    }
}

/// Extract the content of the `## Agent Instructions` section from a
/// task detail markdown string. Returns empty string if not found.
pub fn extract_task_instructions(detail: &str) -> String {
    let mut in_section = false;
    let mut lines: Vec<&str> = Vec::new();

    for line in detail.lines() {
        if line
            .trim()
            .to_lowercase()
            .starts_with("## agent instructions")
        {
            in_section = true;
            continue;
        }
        if in_section {
            // Stop at next `##` heading
            if line.trim_start().starts_with("## ") {
                break;
            }
            lines.push(line);
        }
    }

    lines.join("\n").trim().to_string()
}

/// Format instructions as a system-prompt appendix block.
/// Returns empty string if `instructions` is empty (no block added).
pub fn as_system_appendix(instructions: &str) -> String {
    if instructions.is_empty() {
        return String::new();
    }
    format!("\n\n## Project-Specific Instructions\n\n{instructions}")
}

// ── `specr instructions show` ─────────────────────────────────────────────────

pub fn show(workdir: &Path) {
    use colored::Colorize;

    println!("\n{}", "=== specr instructions ===".bold().cyan());

    let global = global_dir();
    println!(
        "\nGlobal dir: {}",
        global
            .as_ref()
            .map(|d| d.display().to_string())
            .unwrap_or_else(|| "(not found)".to_string())
            .dimmed()
    );
    println!(
        "Project dir: {}\n",
        project_dir(workdir).display().to_string().dimmed()
    );

    for &kind in AgentKind::all() {
        let g = global_dir()
            .and_then(|d| read_file_opt(&d.join(kind.filename())))
            .unwrap_or_default();
        let p = read_file_opt(&project_dir(workdir).join(kind.filename())).unwrap_or_default();
        let combined = combine(&g, &p);

        print!("  {:<18}", format!("{}:", kind.label()).bold());
        if combined.is_empty() {
            println!("{}", "(none)".dimmed());
        } else {
            let preview: String = combined
                .lines()
                .next()
                .unwrap_or("")
                .chars()
                .take(72)
                .collect();
            let lines = combined.lines().count();
            println!(
                "{} {} {}",
                preview.green(),
                format!("[{} lines]", lines).dimmed(),
                if !g.is_empty() && !p.is_empty() {
                    "(global+project)".dimmed()
                } else if !g.is_empty() {
                    "(global)".dimmed()
                } else {
                    "(project)".dimmed()
                }
            );
        }
    }
    println!();
}

// ── `specr instructions init` ────────────────────────────────────────────────

const INIT_CODER: &str = "\
# Coder Instructions
# Add project-specific coding rules here.
# These are appended to the coding agent's system prompt.
#
# Examples:
#   - Always use rustls for TLS, never native-tls or openssl.
#   - Never use unwrap() in production paths; use anyhow::Context instead.
#   - Use tokio::spawn for async concurrency; no std::thread.
";

const INIT_CODE_REVIEWER: &str = "\
# Code Reviewer Instructions
# Customize how the code reviewer evaluates changes.
#
# Examples:
#   - Focus on error handling and edge cases.
#   - Don't require doc comments on private functions.
#   - Fail only on panic paths, unsafe code, or leaked secrets.
";

const INIT_QA_REVIEWER: &str = "\
# QA Reviewer Instructions
# Customize test coverage and quality expectations.
#
# Examples:
#   - Target 80% line coverage (not 90%).
#   - All async tests must use #[tokio::test].
#   - Integration tests live in tests/, unit tests inline.
";

const INIT_STYLE_REVIEWER: &str = "\
# Style Reviewer Instructions
# Customize naming, formatting, and style expectations.
#
# Examples:
#   - Use snake_case for all identifiers; camelCase only in proto definitions.
#   - Module names must be lowercase.
#   - Prefer explicit return over trailing expression in public functions.
";

const INIT_COORDINATOR: &str = "\
# Coordinator Instructions
# Customize how the coordinator resolves merge conflicts and test failures.
#
# Examples:
#   - When resolving conflicts, prefer the implementation with better error handling.
#   - Do not restructure existing module hierarchy during conflict resolution.
#   - Preserve all existing test cases when applying fixes.
";

pub fn init(workdir: &Path) -> Result<()> {
    use colored::Colorize;

    let dir = project_dir(workdir);
    std::fs::create_dir_all(&dir)?;

    let files = [
        ("coder.md", INIT_CODER),
        ("code-reviewer.md", INIT_CODE_REVIEWER),
        ("qa-reviewer.md", INIT_QA_REVIEWER),
        ("style-reviewer.md", INIT_STYLE_REVIEWER),
        ("coordinator.md", INIT_COORDINATOR),
    ];

    let mut created = 0;
    let mut skipped = 0;

    for (name, content) in &files {
        let path = dir.join(name);
        if path.exists() {
            println!("  {} {} (already exists)", "skip".yellow(), name);
            skipped += 1;
        } else {
            std::fs::write(&path, content)?;
            println!("  {} {}", "created".green(), name);
            created += 1;
        }
    }

    println!(
        "\n{} .specr/ initialised ({} created, {} skipped)",
        "Done!".bold().green(),
        created,
        skipped
    );
    println!(
        "Edit {} to add project-specific instructions.\n",
        dir.display()
    );
    Ok(())
}

// ── `specr instructions generate` ─────────────────────────────────────────────

const GENERATE_SYSTEM: &str = "\
You are a senior software architect. Given a project SPEC.md, generate tailored \
instructions for each agent type in an AI-driven coding workflow. The instructions \
should be specific to this project's stack, patterns, and constraints — not generic advice.

Output a JSON object with exactly these keys (values are markdown strings):
{
  \"coder\": \"...\",
  \"code_reviewer\": \"...\",
  \"qa_reviewer\": \"...\",
  \"style_reviewer\": \"...\",
  \"coordinator\": \"...\"
}

Be concise (3-8 bullet points per agent). Focus on what's unique to this project.
Output JSON only, no commentary.";

pub async fn generate(workdir: &Path, config: &Config, client: &dyn LlmClient) -> Result<()> {
    use colored::Colorize;

    let spec = crate::store::read_spec(workdir)?;

    println!("{}", "Generating instructions from SPEC.md...".cyan());

    let response = client.complete(GENERATE_SYSTEM, &spec).await?;

    // Parse JSON
    let raw = {
        let t = response.trim();
        if let (Some(s), Some(e)) = (t.find('{'), t.rfind('}')) {
            t[s..=e].to_string()
        } else {
            t.to_string()
        }
    };

    #[derive(serde::Deserialize)]
    struct Generated {
        coder: String,
        code_reviewer: String,
        qa_reviewer: String,
        style_reviewer: String,
        coordinator: String,
    }

    let gen: Generated = serde_json::from_str(&raw)
        .map_err(|e| anyhow::anyhow!("Failed to parse LLM response: {e}\n\nRaw:\n{raw}"))?;

    let dir = project_dir(workdir);
    std::fs::create_dir_all(&dir)?;

    let files = [
        ("coder.md", gen.coder),
        ("code-reviewer.md", gen.code_reviewer),
        ("qa-reviewer.md", gen.qa_reviewer),
        ("style-reviewer.md", gen.style_reviewer),
        ("coordinator.md", gen.coordinator),
    ];

    for (name, content) in files {
        let path = dir.join(name);
        let existed = path.exists();
        std::fs::write(&path, &content)?;
        let verb = if existed { "updated" } else { "created" };
        println!("  {} {}", verb.green(), name);
    }

    println!(
        "\n{} Instructions generated in .specr/",
        "Done!".bold().green()
    );
    println!("Review and edit before running agents.\n");

    // Show summary
    let _ = config; // available for future config use
    show(workdir);

    Ok(())
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_task_instructions_present() {
        let detail = "## Overview\nDo something.\n\n## Agent Instructions\nUse rustls.\nNo unwrap.\n\n## Next Section\nOther stuff.";
        let result = extract_task_instructions(detail);
        assert_eq!(result, "Use rustls.\nNo unwrap.");
    }

    #[test]
    fn test_extract_task_instructions_absent() {
        let detail = "## Overview\nDo something.";
        assert!(extract_task_instructions(detail).is_empty());
    }

    #[test]
    fn test_extract_task_instructions_at_end() {
        let detail = "## Overview\nFoo.\n\n## Agent Instructions\nUse tokio.";
        let result = extract_task_instructions(detail);
        assert_eq!(result, "Use tokio.");
    }

    #[test]
    fn test_extract_task_instructions_case_insensitive() {
        let detail = "## AGENT INSTRUCTIONS\nBe strict.";
        assert!(!extract_task_instructions(detail).is_empty());
    }

    #[test]
    fn test_combine_both_empty() {
        assert!(combine("", "").is_empty());
    }

    #[test]
    fn test_combine_only_global() {
        assert_eq!(combine("global rule", ""), "global rule");
    }

    #[test]
    fn test_combine_only_project() {
        assert_eq!(combine("", "project rule"), "project rule");
    }

    #[test]
    fn test_combine_both() {
        let result = combine("global", "project");
        assert!(result.contains("global"));
        assert!(result.contains("project"));
        // Global comes first
        assert!(result.find("global").unwrap() < result.find("project").unwrap());
    }

    #[test]
    fn test_as_system_appendix_empty() {
        assert!(as_system_appendix("").is_empty());
    }

    #[test]
    fn test_as_system_appendix_non_empty() {
        let result = as_system_appendix("Use rustls.");
        assert!(result.contains("Project-Specific Instructions"));
        assert!(result.contains("Use rustls."));
    }

    #[test]
    fn test_agent_kind_filenames_unique() {
        let names: Vec<&str> = AgentKind::all().iter().map(|k| k.filename()).collect();
        let mut deduped = names.clone();
        deduped.dedup();
        assert_eq!(names.len(), deduped.len(), "filename collision");
    }

    #[test]
    fn test_agent_kind_all_covered() {
        assert_eq!(AgentKind::all().len(), 5);
    }

    #[test]
    fn test_load_no_files_returns_empty() {
        // Non-existent workdir → no files → empty string
        let result = load(
            Path::new("/tmp/__nonexistent_specr_test__"),
            AgentKind::Coder,
        );
        assert!(result.is_empty());
    }

    #[test]
    fn test_load_project_file() {
        // Create a temp .specr dir with a coder.md
        let dir = std::env::temp_dir().join("specr_instructions_test");
        let specr_dir = dir.join(".specr");
        std::fs::create_dir_all(&specr_dir).unwrap();
        std::fs::write(specr_dir.join("coder.md"), "Use rustls.").unwrap();

        let result = load(&dir, AgentKind::Coder);
        assert!(result.contains("Use rustls."));

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn test_load_with_task_combines_all() {
        let dir = std::env::temp_dir().join("specr_instructions_task_test");
        let specr_dir = dir.join(".specr");
        std::fs::create_dir_all(&specr_dir).unwrap();
        std::fs::write(specr_dir.join("coder.md"), "Project rule.").unwrap();

        let detail = "## Overview\nDo something.\n\n## Agent Instructions\nTask rule.";
        let result = load_with_task(&dir, AgentKind::Coder, detail);

        assert!(result.contains("Project rule."));
        assert!(result.contains("Task rule."));

        std::fs::remove_dir_all(&dir).ok();
    }
}
