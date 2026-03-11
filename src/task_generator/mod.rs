pub mod graph;
pub mod renderer;

use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::{Context, Result};
use colored::Colorize;

use crate::config::Config;
use crate::llm::LlmClient;
use crate::store;
use crate::types::{Task, TaskSize, TaskStatus};

use self::graph::DependencyGraph;
use self::renderer::{extract_project_name, extract_spec_version};

const TASK_DECOMPOSE_SYSTEM: &str = "\
You are a senior software architect. Given a SPEC.md, decompose the project into an ordered, \
dependency-aware list of tasks for an agentic coding workflow.

Rules:
- Each task must produce ONE verifiable output
- Size tasks: S (<2h), M (~half day), L (>half day, must be split)
- Make dependencies explicit (task IDs)
- Done-when must be machine-checkable
- No more than 20 tasks for a typical project
- Output JSON only, no commentary

Output a JSON array of objects with this schema:
[
  {
    \"id\": \"001\",
    \"name\": \"Scaffold project structure\",
    \"size\": \"S\",
    \"depends_on\": [],
    \"done_when\": \"cargo build passes on empty project\",
    \"scope\": \"Create Cargo.toml, src/main.rs, .gitignore, CI config\",
    \"files_to_touch\": [\"Cargo.toml\", \"src/main.rs\"],
    \"not_to_change\": [],
    \"interface\": null
  }
]";

const SPLIT_SYSTEM: &str = "\
You are a senior software architect. Given a large task (size L), split it into smaller subtasks \
(size S or M). Preserve the original task's dependency chain. Each subtask must produce one \
verifiable output. Output JSON only, no commentary.

Output a JSON array with the same schema as task decomposition.";

/// Run the full task generation pipeline.
pub async fn run(config: &Config, client: &dyn LlmClient) -> Result<()> {
    run_with_io(config, client, &mut io::stdin().lock(), &mut io::stdout()).await
}

/// Task generation with injectable I/O for testing.
pub async fn run_with_io<R: BufRead, W: Write>(
    config: &Config,
    client: &dyn LlmClient,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    let dir = std::env::current_dir()?;

    // Step 1: Read SPEC.md
    let spec_content = store::read_spec(&dir)?;
    let spec_version = extract_spec_version(&spec_content);
    let project_name = extract_project_name(&spec_content);

    writeln!(writer, "\n{}", "=== specr tasks ===".bold().cyan())?;
    writeln!(writer, "Project: {}  spec-version: {}\n", project_name.bold(), spec_version)?;

    // Step 2: Check for existing TASKS.md
    let existing_tasks = store::read_tasks(&dir).ok();
    let mut preserved_done: Vec<Task> = Vec::new();
    let mut in_progress_warnings: Vec<String> = Vec::new();

    if let Some((ref old_tasks, old_version)) = existing_tasks {
        if old_version == spec_version {
            writeln!(writer, "TASKS.md already exists with same spec-version ({}).", spec_version)?;
            write!(writer, "Regenerate? (y/n): ")?;
            writer.flush()?;
            let mut input = String::new();
            reader.read_line(&mut input)?;
            if !input.trim().eq_ignore_ascii_case("y") {
                writeln!(writer, "{}", "Aborted.".yellow())?;
                return Ok(());
            }
        } else {
            writeln!(
                writer,
                "Spec version changed ({} -> {}). Applying drift policy...",
                old_version, spec_version
            )?;
            // Drift policy
            for task in old_tasks {
                match task.status {
                    TaskStatus::Done => {
                        preserved_done.push(task.clone());
                    }
                    TaskStatus::InProgress => {
                        in_progress_warnings.push(format!(
                            "  {} {} \u{00b7} {} (in-progress, must be manually resolved)",
                            "WARNING:".yellow(),
                            task.id,
                            task.name
                        ));
                    }
                    TaskStatus::Failed | TaskStatus::Open => {
                        // Will be regenerated
                    }
                }
            }
        }
    }

    // Step 3: Call LLM
    writeln!(writer, "{}", "Decomposing spec into tasks via LLM...".cyan())?;
    let llm_response = client.complete(TASK_DECOMPOSE_SYSTEM, &spec_content).await?;

    // Parse JSON response
    let raw_json = extract_json(&llm_response);
    let mut tasks: Vec<Task> = parse_task_json(&raw_json)
        .context("Failed to parse LLM task decomposition response")?;

    // Re-number tasks to avoid conflicts with preserved done tasks
    if !preserved_done.is_empty() {
        let max_existing: u32 = preserved_done
            .iter()
            .filter_map(|t| t.id.parse::<u32>().ok())
            .max()
            .unwrap_or(0);

        // Build old->new ID mapping
        let id_map: Vec<(String, String)> = tasks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                (t.id.clone(), format!("{:03}", max_existing + 1 + i as u32))
            })
            .collect();

        // Apply mapping
        for task in tasks.iter_mut() {
            if let Some((_, new_id)) = id_map.iter().find(|(old, _)| old == &task.id) {
                task.id = new_id.clone();
                task.branch = Task::default_branch(new_id, &task.name);
            }
            for dep in task.depends_on.iter_mut() {
                if let Some((_, new_id)) = id_map.iter().find(|(old, _)| old == dep) {
                    *dep = new_id.clone();
                }
            }
        }
    }

    // Combine preserved done tasks with new tasks
    let mut all_tasks = preserved_done;
    all_tasks.extend(tasks);

    // Step 4: Validate dependency graph
    let graph = DependencyGraph::build(all_tasks.clone())
        .context("Failed to build dependency graph")?;
    graph.validate().context("Dependency graph validation failed")?;

    // Step 5: Show warnings for in-progress tasks
    if !in_progress_warnings.is_empty() {
        writeln!(writer, "\n{}", "Drift warnings:".yellow().bold())?;
        for w in &in_progress_warnings {
            writeln!(writer, "{}", w)?;
        }
        writeln!(writer)?;
    }

    // Step 6: Print task list for review
    writeln!(writer, "\n{}\n", "--- Proposed Tasks ---".bold().green())?;
    for task in &all_tasks {
        let size_badge = match task.size {
            TaskSize::S => "[S]".green(),
            TaskSize::M => "[M]".yellow(),
            TaskSize::L => "[L]".red(),
        };
        let status_badge = match task.status {
            TaskStatus::Done => "done".green(),
            TaskStatus::Open => "open".normal(),
            TaskStatus::InProgress => "in-progress".cyan(),
            TaskStatus::Failed => "failed".red(),
        };
        let deps = if task.depends_on.is_empty() {
            "\u{2014}".to_string()
        } else {
            task.depends_on.join(", ")
        };
        writeln!(
            writer,
            "  {} {} \u{00b7} {} {}  [{}]  deps: {}",
            task.id, size_badge, task.name, status_badge, task.done_when.dimmed(), deps
        )?;

        if task.size == TaskSize::L {
            writeln!(writer, "    {} This task is size L and should be split.", "!".red().bold())?;
        }
    }

    // Step 7: Show parallel execution groups
    let levels = graph.parallel_safe();
    writeln!(writer, "\n{}", "Parallel execution groups:".dimmed())?;
    for (i, level) in levels.iter().enumerate() {
        let ids: Vec<&str> = level.iter().map(|t| t.id.as_str()).collect();
        writeln!(writer, "  Level {}: {}", i + 1, ids.join(", "))?;
    }

    writeln!(writer)?;

    // Step 8: Approval gate
    writeln!(writer, "{}", "Approve? (yes / edit / no)".bold())?;
    write!(writer, "> ")?;
    writer.flush()?;

    let mut input = String::new();
    reader.read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if input == "yes" || input == "y" {
        // Step 9: Write files
        write_task_files(&dir, &all_tasks, spec_version, &project_name, config, writer)?;
        return Ok(());
    } else if input == "no" || input == "n" {
        writeln!(writer, "{}", "Aborted. No files written.".yellow())?;
        return Ok(());
    } else if input == "edit" {
        writeln!(
            writer,
            "{}",
            "Manual editing not yet implemented. Re-run with updated SPEC.md.".yellow()
        )?;
        return Ok(());
    }

    writeln!(writer, "{}", "Unknown input. Aborting.".red())?;
    Ok(())
}

/// Run the task splitting pipeline for a specific task.
pub async fn split(config: &Config, client: &dyn LlmClient, task_id: &str) -> Result<()> {
    split_with_io(config, client, task_id, &mut io::stdin().lock(), &mut io::stdout()).await
}

/// Split with injectable I/O for testing.
pub async fn split_with_io<R: BufRead, W: Write>(
    _config: &Config,
    client: &dyn LlmClient,
    task_id: &str,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    let dir = std::env::current_dir()?;

    // Read existing tasks
    let (mut tasks, spec_version) = store::read_tasks(&dir)?;

    // Find the target task
    let task_idx = tasks
        .iter()
        .position(|t| t.id == task_id)
        .context(format!("Task {} not found in TASKS.md", task_id))?;

    let task = &tasks[task_idx];
    if task.size != TaskSize::L {
        anyhow::bail!("Task {} is size {}, not L. Only L tasks can be split.", task_id, task.size);
    }

    writeln!(writer, "\n{}", "=== specr split ===".bold().cyan())?;
    writeln!(writer, "Splitting task {} \u{00b7} {}\n", task.id.bold(), task.name)?;
    writeln!(writer, "Current scope: {}\n", task.scope)?;

    // Call LLM to suggest subtasks
    writeln!(writer, "{}", "Generating subtask suggestions via LLM...".cyan())?;

    let prompt = format!(
        "Split this task into smaller subtasks:\n\nID: {}\nName: {}\nScope: {}\nDone when: {}\nDepends on: {}\n\nFiles to touch: {}\n\nGenerate subtask IDs as {}a, {}b, {}c, etc.",
        task.id, task.name, task.scope, task.done_when,
        if task.depends_on.is_empty() { "none".to_string() } else { task.depends_on.join(", ") },
        task.files_to_touch.join(", "),
        task.id, task.id, task.id
    );

    let llm_response = client.complete(SPLIT_SYSTEM, &prompt).await?;
    let raw_json = extract_json(&llm_response);
    let subtasks: Vec<Task> = parse_task_json(&raw_json)
        .context("Failed to parse LLM split response")?;

    // Show subtasks
    writeln!(writer, "\n{}\n", "--- Suggested Subtasks ---".bold().green())?;
    for st in &subtasks {
        writeln!(
            writer,
            "  {} \u{00b7} {} [{}]  done when: {}",
            st.id, st.name, st.size, st.done_when
        )?;
    }

    writeln!(writer, "\n{}", "Approve split? (yes / no)".bold())?;
    write!(writer, "> ")?;
    writer.flush()?;

    let mut input = String::new();
    reader.read_line(&mut input)?;
    let input = input.trim().to_lowercase();

    if input == "yes" || input == "y" {
        // Mark original task as done with "split" note
        tasks[task_idx].status = TaskStatus::Done;

        // Insert subtasks after the original task
        let insert_pos = task_idx + 1;
        for (i, st) in subtasks.into_iter().enumerate() {
            tasks.insert(insert_pos + i, st);
        }

        // Read spec for project name
        let spec_content = store::read_spec(&dir)?;
        let project_name = extract_project_name(&spec_content);

        store::write_tasks(&dir, &tasks, spec_version, &project_name)?;

        // Write detail files for M/L subtasks
        for task in &tasks {
            if task.has_detail_file() {
                store::write_task_detail(&dir, task)?;
            }
        }

        writeln!(writer, "\n{}", "Task split successfully. TASKS.md updated.".green())?;
    } else {
        writeln!(writer, "{}", "Split cancelled.".yellow())?;
    }

    Ok(())
}

/// Write TASKS.md and detail files.
fn write_task_files<W: Write>(
    dir: &Path,
    tasks: &[Task],
    spec_version: u32,
    project_name: &str,
    _config: &Config,
    writer: &mut W,
) -> Result<()> {
    store::write_tasks(dir, tasks, spec_version, project_name)?;

    let mut detail_count = 0;
    for task in tasks {
        if task.has_detail_file() {
            store::write_task_detail(dir, task)?;
            detail_count += 1;
        }
    }

    writeln!(
        writer,
        "\n{} TASKS.md written with {} tasks ({} detail files).",
        "Done!".green().bold(),
        tasks.len(),
        detail_count
    )?;

    Ok(())
}

/// Extract JSON array from LLM response (strips markdown fences if present).
fn extract_json(response: &str) -> String {
    let trimmed = response.trim();

    // Try stripping ```json ... ``` fences
    if let Some(start) = trimmed.find('[') {
        if let Some(end) = trimmed.rfind(']') {
            return trimmed[start..=end].to_string();
        }
    }

    trimmed.to_string()
}

/// Parse the JSON task array from LLM output into Task structs.
fn parse_task_json(json: &str) -> Result<Vec<Task>> {
    #[derive(serde::Deserialize)]
    struct RawTask {
        id: String,
        name: String,
        size: String,
        depends_on: Vec<String>,
        done_when: String,
        scope: Option<String>,
        files_to_touch: Option<Vec<String>>,
        not_to_change: Option<Vec<String>>,
        interface: Option<String>,
    }

    let raw_tasks: Vec<RawTask> =
        serde_json::from_str(json).context("Failed to parse task JSON")?;

    let tasks: Vec<Task> = raw_tasks
        .into_iter()
        .map(|rt| {
            let size: TaskSize = rt.size.parse().unwrap_or(TaskSize::M);
            let branch = Task::default_branch(&rt.id, &rt.name);
            Task {
                id: rt.id,
                name: rt.name,
                size,
                status: TaskStatus::Open,
                depends_on: rt.depends_on,
                done_when: rt.done_when,
                scope: rt.scope.unwrap_or_default(),
                files_to_touch: rt.files_to_touch.unwrap_or_default(),
                not_to_change: rt.not_to_change.unwrap_or_default(),
                branch,
                interface: rt.interface,
            }
        })
        .collect();

    Ok(tasks)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_json_plain() {
        let input = r#"[{"id": "001"}]"#;
        assert_eq!(extract_json(input), r#"[{"id": "001"}]"#);
    }

    #[test]
    fn test_extract_json_with_fences() {
        let input = "```json\n[{\"id\": \"001\"}]\n```";
        assert_eq!(extract_json(input), "[{\"id\": \"001\"}]");
    }

    #[test]
    fn test_extract_json_with_surrounding_text() {
        let input = "Here are the tasks:\n[{\"id\": \"001\"}]\nDone!";
        assert_eq!(extract_json(input), "[{\"id\": \"001\"}]");
    }

    #[test]
    fn test_parse_task_json_basic() {
        let json = r#"[
            {
                "id": "001",
                "name": "Scaffold",
                "size": "S",
                "depends_on": [],
                "done_when": "cargo build passes",
                "scope": "Create project",
                "files_to_touch": ["Cargo.toml"],
                "not_to_change": [],
                "interface": null
            }
        ]"#;
        let tasks = parse_task_json(json).unwrap();
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].id, "001");
        assert_eq!(tasks[0].name, "Scaffold");
        assert_eq!(tasks[0].size, TaskSize::S);
        assert_eq!(tasks[0].status, TaskStatus::Open);
        assert!(tasks[0].depends_on.is_empty());
    }

    #[test]
    fn test_parse_task_json_with_deps() {
        let json = r#"[
            {
                "id": "001",
                "name": "First",
                "size": "S",
                "depends_on": [],
                "done_when": "passes"
            },
            {
                "id": "002",
                "name": "Second",
                "size": "M",
                "depends_on": ["001"],
                "done_when": "passes"
            }
        ]"#;
        let tasks = parse_task_json(json).unwrap();
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[1].depends_on, vec!["001"]);
    }

    #[test]
    fn test_parse_task_json_invalid() {
        let json = "not json at all";
        assert!(parse_task_json(json).is_err());
    }

    #[test]
    fn test_parse_task_json_optional_fields() {
        let json = r#"[
            {
                "id": "001",
                "name": "Scaffold",
                "size": "S",
                "depends_on": [],
                "done_when": "passes"
            }
        ]"#;
        let tasks = parse_task_json(json).unwrap();
        assert_eq!(tasks[0].scope, "");
        assert!(tasks[0].files_to_touch.is_empty());
        assert!(tasks[0].interface.is_none());
    }
}
