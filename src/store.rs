use anyhow::{Context, Result};
use std::path::Path;

use crate::config::Config;
use crate::task_generator::renderer;
use crate::types::{Task, TaskStatus};

const SPEC_FILENAME: &str = "SPEC.md";
const TASKS_FILENAME: &str = "TASKS.md";
const TASKS_DIR: &str = "tasks";

/// Read SPEC.md from the given directory.
pub fn read_spec(dir: &Path) -> Result<String> {
    let path = dir.join(SPEC_FILENAME);
    std::fs::read_to_string(&path).with_context(|| {
        format!(
            "No SPEC.md found in {}. Run: specr compose \"<idea>\"",
            dir.display()
        )
    })
}

/// Write SPEC.md to the given directory.
/// If obsidian_dir is configured, also writes a copy there.
pub fn write_spec(dir: &Path, content: &str, config: &Config) -> Result<()> {
    let path = dir.join(SPEC_FILENAME);
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write SPEC.md to {}", path.display()))?;

    // Write to Obsidian vault if configured
    if !config.output.obsidian_dir.is_empty() {
        let project_name = extract_project_name(content);
        let obsidian_path = Path::new(&config.output.obsidian_dir)
            .join("01_Projects")
            .join(&project_name);
        std::fs::create_dir_all(&obsidian_path).with_context(|| {
            format!(
                "Failed to create Obsidian directory {}",
                obsidian_path.display()
            )
        })?;
        let obsidian_file = obsidian_path.join(SPEC_FILENAME);
        std::fs::write(&obsidian_file, content).with_context(|| {
            format!(
                "Failed to write SPEC.md to Obsidian vault at {}",
                obsidian_file.display()
            )
        })?;
    }

    Ok(())
}

/// Extract project name from SPEC.md content by looking for "# Project: <name>".
pub fn extract_project_name(content: &str) -> String {
    for line in content.lines() {
        if let Some(name) = line.strip_prefix("# Project: ") {
            let name = name.trim();
            if !name.is_empty() {
                return name.to_string();
            }
        }
    }
    "unnamed".to_string()
}

/// Read TASKS.md from the given directory, parsing it into tasks and spec-version.
pub fn read_tasks(dir: &Path) -> Result<(Vec<Task>, u32)> {
    let path = dir.join(TASKS_FILENAME);
    let content = std::fs::read_to_string(&path).with_context(|| {
        format!(
            "No TASKS.md found in {}. Run: specr tasks",
            dir.display()
        )
    })?;
    renderer::parse_tasks_md(&content)
}

/// Write TASKS.md to the given directory.
pub fn write_tasks(dir: &Path, tasks: &[Task], spec_version: u32, project_name: &str) -> Result<()> {
    let content = renderer::render_tasks_md(tasks, spec_version, project_name);
    let path = dir.join(TASKS_FILENAME);
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write TASKS.md to {}", path.display()))
}

/// Write a per-task detail file (for M and L tasks).
pub fn write_task_detail(dir: &Path, task: &Task) -> Result<()> {
    let tasks_dir = dir.join(TASKS_DIR);
    std::fs::create_dir_all(&tasks_dir)
        .with_context(|| format!("Failed to create tasks directory {}", tasks_dir.display()))?;

    let filename = task.detail_filename();
    // detail_filename() returns "tasks/NNN-name.md", so strip the "tasks/" prefix
    let relative_name = filename.strip_prefix("tasks/").unwrap_or(&filename);
    let path = tasks_dir.join(relative_name);
    let content = renderer::render_task_detail(task);
    std::fs::write(&path, content)
        .with_context(|| format!("Failed to write task detail to {}", path.display()))
}

/// Read a task detail file by task ID (searches for files matching the ID prefix).
pub fn read_task_detail(dir: &Path, task_id: &str) -> Result<String> {
    let tasks_dir = dir.join(TASKS_DIR);
    if !tasks_dir.exists() {
        anyhow::bail!("No tasks directory found in {}", dir.display());
    }

    let entries = std::fs::read_dir(&tasks_dir)
        .with_context(|| format!("Failed to read tasks directory {}", tasks_dir.display()))?;

    for entry in entries {
        let entry = entry?;
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with(task_id) && name_str.ends_with(".md") {
            return std::fs::read_to_string(entry.path())
                .with_context(|| format!("Failed to read {}", entry.path().display()));
        }
    }

    anyhow::bail!("No detail file found for task {}", task_id)
}

/// Update a task's status in TASKS.md on disk.
pub fn update_task_status(dir: &Path, task_id: &str, new_status: TaskStatus) -> Result<()> {
    let (mut tasks, version) = read_tasks(dir)?;
    let spec_content = read_spec(dir).unwrap_or_default();
    let project_name = extract_project_name(&spec_content);

    let task = tasks
        .iter_mut()
        .find(|t| t.id == task_id)
        .with_context(|| format!("Task {} not found in TASKS.md", task_id))?;
    task.status = new_status;

    write_tasks(dir, &tasks, version, &project_name)
}

/// Increment the spec-version in SPEC.md frontmatter.
/// Looks for `spec-version: N` and replaces with `spec-version: N+1`.
/// Also updates the `updated:` date to today.
pub fn bump_spec_version(content: &str) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let mut result = String::with_capacity(content.len());
    let mut version_bumped = false;

    for line in content.lines() {
        if line.starts_with("spec-version:") && !version_bumped {
            // Parse current version, increment it
            if let Some(version_str) = line.strip_prefix("spec-version:") {
                if let Ok(version) = version_str.trim().parse::<u32>() {
                    result.push_str(&format!("spec-version: {}", version + 1));
                    version_bumped = true;
                } else {
                    result.push_str(line);
                }
            } else {
                result.push_str(line);
            }
        } else if line.starts_with("updated:") {
            result.push_str(&format!("updated: {}", today));
        } else {
            result.push_str(line);
        }
        result.push('\n');
    }

    // Remove trailing newline if original didn't have one
    if !content.ends_with('\n') && result.ends_with('\n') {
        result.pop();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config() -> Config {
        Config::default()
    }

    #[test]
    fn test_write_and_read_spec() {
        let tmp = TempDir::new().unwrap();
        let content = "# Project: Test\n\nSome content";
        write_spec(tmp.path(), content, &test_config()).unwrap();

        let read_content = read_spec(tmp.path()).unwrap();
        assert_eq!(read_content, content);
    }

    #[test]
    fn test_read_spec_missing() {
        let tmp = TempDir::new().unwrap();
        let result = read_spec(tmp.path());
        assert!(result.is_err());
        let err_msg = result.unwrap_err().to_string();
        assert!(err_msg.contains("No SPEC.md found"));
        assert!(err_msg.contains("specr compose"));
    }

    #[test]
    fn test_write_spec_with_obsidian() {
        let tmp = TempDir::new().unwrap();
        let obsidian_dir = tmp.path().join("vault");
        std::fs::create_dir_all(&obsidian_dir).unwrap();

        let mut config = test_config();
        config.output.obsidian_dir = obsidian_dir.to_string_lossy().to_string();

        let content = "# Project: MyApp\n\nSome content";
        write_spec(tmp.path(), content, &config).unwrap();

        // Check main file
        let main_spec = tmp.path().join("SPEC.md");
        assert!(main_spec.exists());

        // Check Obsidian copy
        let obsidian_spec = obsidian_dir
            .join("01_Projects")
            .join("MyApp")
            .join("SPEC.md");
        assert!(obsidian_spec.exists());
        let obsidian_content = std::fs::read_to_string(obsidian_spec).unwrap();
        assert_eq!(obsidian_content, content);
    }

    #[test]
    fn test_extract_project_name() {
        assert_eq!(extract_project_name("# Project: Todo API"), "Todo API");
        assert_eq!(extract_project_name("no project header"), "unnamed");
        assert_eq!(extract_project_name("# Project: "), "unnamed");
    }

    #[test]
    fn test_bump_spec_version() {
        let content = "---\nspec-version: 1\ncreated: 2024-01-01\nupdated: 2024-01-01\n---\n\n# Project: Test";
        let bumped = bump_spec_version(content);
        assert!(bumped.contains("spec-version: 2"));
        assert!(!bumped.contains("spec-version: 1"));
        // updated date should change
        assert!(!bumped.contains("updated: 2024-01-01"));
    }

    #[test]
    fn test_bump_spec_version_higher_number() {
        let content = "---\nspec-version: 42\ncreated: 2024-01-01\nupdated: 2024-01-01\n---";
        let bumped = bump_spec_version(content);
        assert!(bumped.contains("spec-version: 43"));
    }

    #[test]
    fn test_bump_spec_version_no_version() {
        let content = "# Just a heading\nNo frontmatter here";
        let bumped = bump_spec_version(content);
        assert_eq!(bumped, content);
    }

    #[test]
    fn test_bump_spec_version_preserves_content() {
        let content = "---\nspec-version: 1\ncreated: 2024-01-01\nupdated: 2024-01-01\n---\n\n# Project: Test\n\n## Goal\nDo things";
        let bumped = bump_spec_version(content);
        assert!(bumped.contains("# Project: Test"));
        assert!(bumped.contains("## Goal"));
        assert!(bumped.contains("Do things"));
    }

    // --- TASKS.md tests ---

    #[test]
    fn test_write_and_read_tasks() {
        let tmp = TempDir::new().unwrap();
        let tasks = vec![
            crate::types::Task {
                id: "001".to_string(),
                name: "Scaffold".to_string(),
                size: crate::types::TaskSize::S,
                status: crate::types::TaskStatus::Open,
                depends_on: vec![],
                done_when: "cargo build passes".to_string(),
                scope: "Create project".to_string(),
                files_to_touch: vec!["Cargo.toml".to_string()],
                not_to_change: vec![],
                branch: "task/001-scaffold".to_string(),
                interface: None,
            },
            crate::types::Task {
                id: "002".to_string(),
                name: "Data models".to_string(),
                size: crate::types::TaskSize::M,
                status: crate::types::TaskStatus::Open,
                depends_on: vec!["001".to_string()],
                done_when: "tests pass".to_string(),
                scope: "Create models".to_string(),
                files_to_touch: vec!["src/models.rs".to_string()],
                not_to_change: vec![],
                branch: "task/002-data-models".to_string(),
                interface: None,
            },
        ];

        write_tasks(tmp.path(), &tasks, 1, "Test Project").unwrap();
        assert!(tmp.path().join("TASKS.md").exists());

        let (read_tasks, version) = read_tasks(tmp.path()).unwrap();
        assert_eq!(version, 1);
        assert_eq!(read_tasks.len(), 2);
        assert_eq!(read_tasks[0].id, "001");
        assert_eq!(read_tasks[1].id, "002");
    }

    #[test]
    fn test_read_tasks_missing() {
        let tmp = TempDir::new().unwrap();
        let result = read_tasks(tmp.path());
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No TASKS.md found"));
    }

    #[test]
    fn test_write_and_read_task_detail() {
        let tmp = TempDir::new().unwrap();
        let task = crate::types::Task {
            id: "002".to_string(),
            name: "Data models".to_string(),
            size: crate::types::TaskSize::M,
            status: crate::types::TaskStatus::Open,
            depends_on: vec!["001".to_string()],
            done_when: "tests pass".to_string(),
            scope: "Create models".to_string(),
            files_to_touch: vec!["src/models.rs".to_string()],
            not_to_change: vec!["README.md".to_string()],
            branch: "task/002-data-models".to_string(),
            interface: Some("pub struct User {}".to_string()),
        };

        write_task_detail(tmp.path(), &task).unwrap();

        let content = read_task_detail(tmp.path(), "002").unwrap();
        assert!(content.contains("Task 002"));
        assert!(content.contains("Data models"));
        assert!(content.contains("**Size:** M"));
        assert!(content.contains("src/models.rs"));
    }

    #[test]
    fn test_read_task_detail_missing() {
        let tmp = TempDir::new().unwrap();
        let result = read_task_detail(tmp.path(), "999");
        assert!(result.is_err());
    }

    #[test]
    fn test_update_task_status() {
        let tmp = TempDir::new().unwrap();
        // Write a SPEC.md so extract_project_name works
        write_spec(tmp.path(), "# Project: Test\n\nContent", &test_config()).unwrap();

        let tasks = vec![
            crate::types::Task {
                id: "001".to_string(),
                name: "Scaffold".to_string(),
                size: crate::types::TaskSize::S,
                status: crate::types::TaskStatus::Open,
                depends_on: vec![],
                done_when: "cargo build passes".to_string(),
                scope: "Create project".to_string(),
                files_to_touch: vec![],
                not_to_change: vec![],
                branch: "task/001-scaffold".to_string(),
                interface: None,
            },
        ];

        write_tasks(tmp.path(), &tasks, 1, "Test").unwrap();
        update_task_status(tmp.path(), "001", TaskStatus::InProgress).unwrap();

        let (updated, _) = read_tasks(tmp.path()).unwrap();
        assert_eq!(updated[0].status, TaskStatus::InProgress);
    }

    #[test]
    fn test_update_task_status_not_found() {
        let tmp = TempDir::new().unwrap();
        write_spec(tmp.path(), "# Project: Test\n\nContent", &test_config()).unwrap();

        let tasks = vec![
            crate::types::Task {
                id: "001".to_string(),
                name: "Scaffold".to_string(),
                size: crate::types::TaskSize::S,
                status: crate::types::TaskStatus::Open,
                depends_on: vec![],
                done_when: "tests pass".to_string(),
                scope: "scope".to_string(),
                files_to_touch: vec![],
                not_to_change: vec![],
                branch: "task/001-scaffold".to_string(),
                interface: None,
            },
        ];

        write_tasks(tmp.path(), &tasks, 1, "Test").unwrap();
        let result = update_task_status(tmp.path(), "999", TaskStatus::Done);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("999"));
    }
}
