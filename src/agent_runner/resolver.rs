use std::collections::HashSet;

use crate::types::{Task, TaskSize, TaskStatus};

/// Resolves which tasks are eligible to run next.
pub struct Resolver;

impl Resolver {
    /// Returns all tasks eligible to run:
    /// - status = Open
    /// - size = S or M (L tasks blocked)
    /// - all dependencies have status = Done
    pub fn eligible(tasks: &[Task]) -> Vec<&Task> {
        let done_ids: HashSet<&str> = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .map(|t| t.id.as_str())
            .collect();

        tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Open
                    && t.size != TaskSize::L
                    && t.depends_on.iter().all(|d| done_ids.contains(d.as_str()))
            })
            .collect()
    }

    /// Returns groups of tasks that can run in parallel.
    /// Tasks in the same group have no shared file dependencies.
    pub fn parallel_groups<'a>(tasks: &'a [Task]) -> Vec<Vec<&'a Task>> {
        let eligible = Self::eligible(tasks);
        if eligible.is_empty() {
            return vec![];
        }

        let mut groups: Vec<Vec<&'a Task>> = Vec::new();

        for task in eligible {
            let task_files: HashSet<&str> =
                task.files_to_touch.iter().map(|f| f.as_str()).collect();

            // Try to find an existing group with no file conflicts
            let mut placed = false;
            for group in &mut groups {
                let group_files: HashSet<&str> = group
                    .iter()
                    .flat_map(|t| t.files_to_touch.iter().map(|f| f.as_str()))
                    .collect();

                if task_files.is_disjoint(&group_files) {
                    group.push(task);
                    placed = true;
                    break;
                }
            }

            if !placed {
                groups.push(vec![task]);
            }
        }

        groups
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_task(id: &str, size: TaskSize, status: TaskStatus, deps: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            name: format!("Task {}", id),
            size,
            status,
            depends_on: deps.into_iter().map(String::from).collect(),
            done_when: "tests pass".to_string(),
            scope: "scope".to_string(),
            files_to_touch: vec![],
            not_to_change: vec![],
            branch: format!("task/{}-task", id),
            interface: None,
        }
    }

    fn make_task_with_files(
        id: &str,
        size: TaskSize,
        status: TaskStatus,
        deps: Vec<&str>,
        files: Vec<&str>,
    ) -> Task {
        let mut t = make_task(id, size, status, deps);
        t.files_to_touch = files.into_iter().map(String::from).collect();
        t
    }

    #[test]
    fn test_eligible_basic_open_tasks() {
        let tasks = vec![
            make_task("001", TaskSize::S, TaskStatus::Open, vec![]),
            make_task("002", TaskSize::M, TaskStatus::Open, vec![]),
        ];
        let eligible = Resolver::eligible(&tasks);
        assert_eq!(eligible.len(), 2);
    }

    #[test]
    fn test_eligible_blocks_l_tasks() {
        let tasks = vec![
            make_task("001", TaskSize::L, TaskStatus::Open, vec![]),
            make_task("002", TaskSize::S, TaskStatus::Open, vec![]),
        ];
        let eligible = Resolver::eligible(&tasks);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "002");
    }

    #[test]
    fn test_eligible_blocks_unmet_deps() {
        let tasks = vec![
            make_task("001", TaskSize::S, TaskStatus::Open, vec![]),
            make_task("002", TaskSize::S, TaskStatus::Open, vec!["001"]),
        ];
        let eligible = Resolver::eligible(&tasks);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "001");
    }

    #[test]
    fn test_eligible_allows_done_deps() {
        let tasks = vec![
            make_task("001", TaskSize::S, TaskStatus::Done, vec![]),
            make_task("002", TaskSize::S, TaskStatus::Open, vec!["001"]),
        ];
        let eligible = Resolver::eligible(&tasks);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "002");
    }

    #[test]
    fn test_eligible_excludes_non_open() {
        let tasks = vec![
            make_task("001", TaskSize::S, TaskStatus::InProgress, vec![]),
            make_task("002", TaskSize::S, TaskStatus::Done, vec![]),
            make_task("003", TaskSize::S, TaskStatus::Failed, vec![]),
        ];
        let eligible = Resolver::eligible(&tasks);
        assert!(eligible.is_empty());
    }

    #[test]
    fn test_eligible_failed_dep_blocks() {
        let tasks = vec![
            make_task("001", TaskSize::S, TaskStatus::Failed, vec![]),
            make_task("002", TaskSize::S, TaskStatus::Open, vec!["001"]),
        ];
        let eligible = Resolver::eligible(&tasks);
        assert!(eligible.is_empty());
    }

    #[test]
    fn test_eligible_empty() {
        let tasks: Vec<Task> = vec![];
        let eligible = Resolver::eligible(&tasks);
        assert!(eligible.is_empty());
    }

    #[test]
    fn test_eligible_chain_only_first() {
        let tasks = vec![
            make_task("001", TaskSize::S, TaskStatus::Open, vec![]),
            make_task("002", TaskSize::S, TaskStatus::Open, vec!["001"]),
            make_task("003", TaskSize::S, TaskStatus::Open, vec!["002"]),
        ];
        let eligible = Resolver::eligible(&tasks);
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "001");
    }

    #[test]
    fn test_parallel_groups_no_file_conflicts() {
        let tasks = vec![
            make_task_with_files("001", TaskSize::S, TaskStatus::Open, vec![], vec!["a.rs"]),
            make_task_with_files("002", TaskSize::S, TaskStatus::Open, vec![], vec!["b.rs"]),
        ];
        let groups = Resolver::parallel_groups(&tasks);
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn test_parallel_groups_file_conflict() {
        let tasks = vec![
            make_task_with_files("001", TaskSize::S, TaskStatus::Open, vec![], vec!["a.rs"]),
            make_task_with_files("002", TaskSize::S, TaskStatus::Open, vec![], vec!["a.rs"]),
        ];
        let groups = Resolver::parallel_groups(&tasks);
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 1);
        assert_eq!(groups[1].len(), 1);
    }

    #[test]
    fn test_parallel_groups_empty_files() {
        let tasks = vec![
            make_task("001", TaskSize::S, TaskStatus::Open, vec![]),
            make_task("002", TaskSize::S, TaskStatus::Open, vec![]),
        ];
        let groups = Resolver::parallel_groups(&tasks);
        // Empty file lists are disjoint, so they can be parallel
        assert_eq!(groups.len(), 1);
        assert_eq!(groups[0].len(), 2);
    }

    #[test]
    fn test_parallel_groups_empty_eligible() {
        let tasks = vec![make_task("001", TaskSize::L, TaskStatus::Open, vec![])];
        let groups = Resolver::parallel_groups(&tasks);
        assert!(groups.is_empty());
    }

    #[test]
    fn test_parallel_groups_mixed() {
        let tasks = vec![
            make_task_with_files("001", TaskSize::S, TaskStatus::Open, vec![], vec!["a.rs"]),
            make_task_with_files("002", TaskSize::S, TaskStatus::Open, vec![], vec!["b.rs"]),
            make_task_with_files(
                "003",
                TaskSize::S,
                TaskStatus::Open,
                vec![],
                vec!["a.rs", "c.rs"],
            ),
        ];
        let groups = Resolver::parallel_groups(&tasks);
        // 001 and 002 can be parallel (different files)
        // 003 conflicts with 001 (a.rs), so goes in a separate group
        assert_eq!(groups.len(), 2);
        assert_eq!(groups[0].len(), 2); // 001 and 002
        assert_eq!(groups[1].len(), 1); // 003
    }
}
