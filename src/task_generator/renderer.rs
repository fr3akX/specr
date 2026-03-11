use anyhow::{Context, Result};
use regex::Regex;

use crate::types::{Task, TaskSize, TaskStatus};

/// Render the complete TASKS.md content from a list of tasks.
pub fn render_tasks_md(tasks: &[Task], spec_version: u32, project_name: &str) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut out = String::new();

    out.push_str("---\n");
    out.push_str(&format!("spec-version: {}\n", spec_version));
    out.push_str(&format!("generated: {}\n", today));
    out.push_str("---\n\n");
    out.push_str(&format!("# Tasks — {}\n\n", project_name));
    out.push_str("## Backlog\n\n");

    for task in tasks {
        let check = if task.status == TaskStatus::Done {
            "x"
        } else {
            " "
        };

        let size_label = match (&task.size, &task.status) {
            (TaskSize::L, TaskStatus::Done) => "[L -> split]".to_string(),
            _ => format!("[{}]", task.size),
        };

        let deps = if task.depends_on.is_empty() {
            "\u{2014}".to_string() // em dash
        } else {
            task.depends_on.join(", ")
        };

        out.push_str(&format!(
            "- [{}] {} \u{00b7} {} {}\n",
            check, task.id, task.name, size_label
        ));
        out.push_str(&format!("      Status: {}\n", task.status));
        out.push_str(&format!("      Depends on: {}\n", deps));
        out.push_str(&format!("      Branch: {}\n", task.branch));
        out.push_str(&format!("      Done when: {}\n", task.done_when));

        if task.has_detail_file() {
            out.push_str(&format!("      Detail: {}\n", task.detail_filename()));
        }

        out.push('\n');
    }

    out
}

/// Render a per-task detail file for M and L tasks.
pub fn render_task_detail(task: &Task) -> String {
    let mut out = String::new();

    out.push_str(&format!("# Task {} \u{00b7} {}\n\n", task.id, task.name));
    out.push_str(&format!("**Size:** {}\n", task.size));
    out.push_str(&format!("**Status:** {}\n", task.status));

    let deps = if task.depends_on.is_empty() {
        "\u{2014}".to_string()
    } else {
        task.depends_on.join(", ")
    };
    out.push_str(&format!("**Depends on:** {}\n", deps));
    out.push_str(&format!("**Branch:** {}\n", task.branch));
    out.push_str(&format!("**Done when:** {}\n\n", task.done_when));

    out.push_str("## Scope\n");
    out.push_str(&task.scope);
    out.push_str("\n\n");

    out.push_str("## Files to touch\n");
    if task.files_to_touch.is_empty() {
        out.push_str("- (none specified)\n");
    } else {
        for f in &task.files_to_touch {
            out.push_str(&format!("- {}\n", f));
        }
    }
    out.push('\n');

    out.push_str("## Interface to implement\n");
    if let Some(iface) = &task.interface {
        out.push_str(iface);
        out.push('\n');
    } else {
        out.push_str("(none specified)\n");
    }
    out.push('\n');

    out.push_str("## What NOT to change\n");
    if task.not_to_change.is_empty() {
        out.push_str("- (none specified)\n");
    } else {
        for f in &task.not_to_change {
            out.push_str(&format!("- {}\n", f));
        }
    }
    out.push('\n');

    out
}

/// Parse a TASKS.md file back into a Vec<Task> and spec-version.
pub fn parse_tasks_md(content: &str) -> Result<(Vec<Task>, u32)> {
    let mut spec_version: u32 = 0;
    let mut tasks: Vec<Task> = Vec::new();

    // Parse frontmatter
    let fm_re = Regex::new(r"(?s)^---\n(.*?)\n---").context("Invalid regex")?;
    if let Some(cap) = fm_re.captures(content) {
        let frontmatter = &cap[1];
        for line in frontmatter.lines() {
            if let Some(v) = line.strip_prefix("spec-version:") {
                spec_version = v.trim().parse().unwrap_or(0);
            }
        }
    }

    // Parse task entries
    // Pattern: - [ ] 001 · Name [S]  or  - [x] 001 · Name [L -> split]
    let task_re = Regex::new(
        r"- \[([x ])\] (\d+) \u{00b7} (.+?) \[([SML])(?: -> split)?\]"
    )
    .context("Invalid task regex")?;

    let lines: Vec<&str> = content.lines().collect();
    let mut i = 0;

    while i < lines.len() {
        if let Some(cap) = task_re.captures(lines[i]) {
            let id = cap[2].to_string();
            let name = cap[3].trim().to_string();
            let size: TaskSize = cap[4].parse()?;

            // Parse indented metadata lines
            let mut status = if &cap[1] == "x" {
                TaskStatus::Done
            } else {
                TaskStatus::Open
            };
            let mut depends_on: Vec<String> = vec![];
            let mut branch = String::new();
            let mut done_when = String::new();

            i += 1;
            while i < lines.len() && lines[i].starts_with("      ") {
                let line = lines[i].trim();
                if let Some(val) = line.strip_prefix("Status: ") {
                    status = val.parse().unwrap_or(status);
                } else if let Some(val) = line.strip_prefix("Depends on: ") {
                    if val != "\u{2014}" {
                        depends_on = val.split(", ").map(|s| s.trim().to_string()).collect();
                    }
                } else if let Some(val) = line.strip_prefix("Branch: ") {
                    branch = val.to_string();
                } else if let Some(val) = line.strip_prefix("Done when: ") {
                    done_when = val.to_string();
                }
                // Skip Detail: line — we don't need it for reconstruction
                i += 1;
            }

            if branch.is_empty() {
                branch = Task::default_branch(&id, &name);
            }

            tasks.push(Task {
                id,
                name: name.clone(),
                size,
                status,
                depends_on,
                done_when,
                scope: String::new(),
                files_to_touch: vec![],
                not_to_change: vec![],
                branch,
                interface: None,
            });
        } else {
            i += 1;
        }
    }

    Ok((tasks, spec_version))
}

/// Extract the spec-version from SPEC.md frontmatter.
pub fn extract_spec_version(spec_content: &str) -> u32 {
    for line in spec_content.lines() {
        if let Some(v) = line.strip_prefix("spec-version:") {
            return v.trim().parse().unwrap_or(1);
        }
    }
    1
}

/// Extract the project name from SPEC.md.
pub fn extract_project_name(spec_content: &str) -> String {
    for line in spec_content.lines() {
        if let Some(name) = line.strip_prefix("# Project: ") {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    "Unnamed Project".to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(id: &str, name: &str, size: TaskSize, deps: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            name: name.to_string(),
            size: size.clone(),
            status: TaskStatus::Open,
            depends_on: deps.into_iter().map(String::from).collect(),
            done_when: "tests pass".to_string(),
            scope: "Do the thing".to_string(),
            files_to_touch: vec!["src/main.rs".to_string()],
            not_to_change: vec!["README.md".to_string()],
            branch: Task::default_branch(id, name),
            interface: Some("fn do_thing()".to_string()),
        }
    }

    #[test]
    fn test_render_tasks_md_basic() {
        let tasks = vec![
            make_task("001", "Scaffold", TaskSize::S, vec![]),
            make_task("002", "Data models", TaskSize::M, vec!["001"]),
        ];
        let md = render_tasks_md(&tasks, 1, "Test Project");
        assert!(md.contains("spec-version: 1"));
        assert!(md.contains("# Tasks \u{2014} Test Project"));
        assert!(md.contains("001 \u{00b7} Scaffold [S]"));
        assert!(md.contains("002 \u{00b7} Data models [M]"));
        assert!(md.contains("Depends on: \u{2014}")); // first task, no deps
        assert!(md.contains("Depends on: 001")); // second task
    }

    #[test]
    fn test_render_tasks_md_done_task() {
        let mut task = make_task("001", "Scaffold", TaskSize::S, vec![]);
        task.status = TaskStatus::Done;
        let md = render_tasks_md(&[task], 1, "Test");
        assert!(md.contains("[x] 001"));
    }

    #[test]
    fn test_render_tasks_md_detail_link() {
        let tasks = vec![
            make_task("001", "Scaffold", TaskSize::S, vec![]),
            make_task("002", "Data models", TaskSize::M, vec!["001"]),
        ];
        let md = render_tasks_md(&tasks, 1, "Test");
        // S task should NOT have Detail line
        assert!(!md.contains("Detail: tasks/001"));
        // M task should have Detail line
        assert!(md.contains("Detail: tasks/002-data-models.md"));
    }

    #[test]
    fn test_render_task_detail() {
        let task = make_task("002", "Data models", TaskSize::M, vec!["001"]);
        let detail = render_task_detail(&task);
        assert!(detail.contains("# Task 002 \u{00b7} Data models"));
        assert!(detail.contains("**Size:** M"));
        assert!(detail.contains("**Status:** open"));
        assert!(detail.contains("**Depends on:** 001"));
        assert!(detail.contains("## Scope"));
        assert!(detail.contains("Do the thing"));
        assert!(detail.contains("- src/main.rs"));
        assert!(detail.contains("fn do_thing()"));
        assert!(detail.contains("- README.md"));
    }

    #[test]
    fn test_render_task_detail_no_deps() {
        let task = make_task("001", "Scaffold", TaskSize::M, vec![]);
        let detail = render_task_detail(&task);
        assert!(detail.contains("**Depends on:** \u{2014}"));
    }

    #[test]
    fn test_render_task_detail_no_interface() {
        let mut task = make_task("001", "Scaffold", TaskSize::M, vec![]);
        task.interface = None;
        let detail = render_task_detail(&task);
        assert!(detail.contains("(none specified)"));
    }

    #[test]
    fn test_parse_tasks_md_roundtrip() {
        let tasks = vec![
            make_task("001", "Scaffold", TaskSize::S, vec![]),
            make_task("002", "Data models", TaskSize::M, vec!["001"]),
        ];
        let md = render_tasks_md(&tasks, 2, "Test Project");
        let (parsed, version) = parse_tasks_md(&md).unwrap();
        assert_eq!(version, 2);
        assert_eq!(parsed.len(), 2);
        assert_eq!(parsed[0].id, "001");
        assert_eq!(parsed[0].name, "Scaffold");
        assert_eq!(parsed[0].size, TaskSize::S);
        assert_eq!(parsed[0].status, TaskStatus::Open);
        assert!(parsed[0].depends_on.is_empty());
        assert_eq!(parsed[1].id, "002");
        assert_eq!(parsed[1].depends_on, vec!["001"]);
    }

    #[test]
    fn test_parse_tasks_md_done_status() {
        let mut task = make_task("001", "Scaffold", TaskSize::S, vec![]);
        task.status = TaskStatus::Done;
        let md = render_tasks_md(&[task], 1, "Test");
        let (parsed, _) = parse_tasks_md(&md).unwrap();
        assert_eq!(parsed[0].status, TaskStatus::Done);
    }

    #[test]
    fn test_parse_tasks_md_failed_status() {
        let mut task = make_task("001", "Scaffold", TaskSize::S, vec![]);
        task.status = TaskStatus::Failed;
        let md = render_tasks_md(&[task], 1, "Test");
        let (parsed, _) = parse_tasks_md(&md).unwrap();
        assert_eq!(parsed[0].status, TaskStatus::Failed);
    }

    #[test]
    fn test_parse_tasks_md_empty() {
        let md = "---\nspec-version: 1\ngenerated: 2024-01-01\n---\n\n# Tasks\n\n## Backlog\n";
        let (parsed, version) = parse_tasks_md(md).unwrap();
        assert_eq!(version, 1);
        assert!(parsed.is_empty());
    }

    #[test]
    fn test_extract_spec_version() {
        let spec = "---\nspec-version: 3\ncreated: 2024-01-01\n---\n\n# Project: Test";
        assert_eq!(extract_spec_version(spec), 3);
    }

    #[test]
    fn test_extract_spec_version_missing() {
        assert_eq!(extract_spec_version("# No frontmatter"), 1);
    }

    #[test]
    fn test_extract_project_name() {
        assert_eq!(
            extract_project_name("---\n---\n\n# Project: My App"),
            "My App"
        );
    }

    #[test]
    fn test_extract_project_name_missing() {
        assert_eq!(extract_project_name("# Something else"), "Unnamed Project");
    }

    #[test]
    fn test_roundtrip_multiple_deps() {
        let task = make_task("005", "Integration tests", TaskSize::S, vec!["003", "004"]);
        let md = render_tasks_md(&[task], 1, "Test");
        let (parsed, _) = parse_tasks_md(&md).unwrap();
        assert_eq!(parsed[0].depends_on, vec!["003", "004"]);
    }

    #[test]
    fn test_roundtrip_in_progress() {
        let mut task = make_task("002", "Work", TaskSize::M, vec!["001"]);
        task.status = TaskStatus::InProgress;
        let md = render_tasks_md(&[task], 1, "Test");
        let (parsed, _) = parse_tasks_md(&md).unwrap();
        assert_eq!(parsed[0].status, TaskStatus::InProgress);
    }
}
