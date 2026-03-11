use anyhow::{bail, Result};
use std::collections::{HashMap, HashSet, VecDeque};

use crate::types::{Task, TaskStatus};

/// A dependency graph over tasks, used for validation and scheduling.
#[derive(Debug)]
pub struct DependencyGraph {
    tasks: Vec<Task>,
}

impl DependencyGraph {
    /// Build a dependency graph from a list of tasks.
    /// Validates that all dependency references point to existing task IDs.
    pub fn build(tasks: Vec<Task>) -> Result<Self> {
        let ids: HashSet<&str> = tasks.iter().map(|t| t.id.as_str()).collect();
        for task in &tasks {
            for dep in &task.depends_on {
                if !ids.contains(dep.as_str()) {
                    bail!("Task {} depends on non-existent task {}", task.id, dep);
                }
            }
        }
        Ok(Self { tasks })
    }

    /// Check for dependency cycles using topological sort (Kahn's algorithm).
    pub fn validate(&self) -> Result<()> {
        let id_to_idx: HashMap<&str, usize> = self
            .tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.as_str(), i))
            .collect();

        let n = self.tasks.len();
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n];

        for (i, task) in self.tasks.iter().enumerate() {
            for dep in &task.depends_on {
                if let Some(&dep_idx) = id_to_idx.get(dep.as_str()) {
                    adj[dep_idx].push(i);
                    in_degree[i] += 1;
                }
            }
        }

        let mut queue: VecDeque<usize> = VecDeque::new();
        for (i, &deg) in in_degree.iter().enumerate() {
            if deg == 0 {
                queue.push_back(i);
            }
        }

        let mut visited = 0;
        while let Some(node) = queue.pop_front() {
            visited += 1;
            for &neighbor in &adj[node] {
                in_degree[neighbor] -= 1;
                if in_degree[neighbor] == 0 {
                    queue.push_back(neighbor);
                }
            }
        }

        if visited != n {
            let cycle_tasks: Vec<&str> = in_degree
                .iter()
                .enumerate()
                .filter(|(_, &deg)| deg > 0)
                .map(|(i, _)| self.tasks[i].id.as_str())
                .collect();
            bail!(
                "Dependency cycle detected involving tasks: {}",
                cycle_tasks.join(", ")
            );
        }

        Ok(())
    }

    /// Return tasks whose dependencies are all done and whose status is open.
    #[allow(dead_code)]
    pub fn eligible_tasks(&self) -> Vec<&Task> {
        let done_ids: HashSet<&str> = self
            .tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .map(|t| t.id.as_str())
            .collect();

        self.tasks
            .iter()
            .filter(|t| {
                t.status == TaskStatus::Open
                    && t.depends_on.iter().all(|d| done_ids.contains(d.as_str()))
            })
            .collect()
    }

    /// Return groups of tasks that can safely run in parallel.
    /// Uses topological levels: each level contains tasks whose deps are all in prior levels.
    pub fn parallel_safe(&self) -> Vec<Vec<&Task>> {
        let id_to_idx: HashMap<&str, usize> = self
            .tasks
            .iter()
            .enumerate()
            .map(|(i, t)| (t.id.as_str(), i))
            .collect();

        let n = self.tasks.len();
        let mut in_degree = vec![0usize; n];
        let mut adj: Vec<Vec<usize>> = vec![vec![]; n];

        for (i, task) in self.tasks.iter().enumerate() {
            for dep in &task.depends_on {
                if let Some(&dep_idx) = id_to_idx.get(dep.as_str()) {
                    adj[dep_idx].push(i);
                    in_degree[i] += 1;
                }
            }
        }

        let mut levels: Vec<Vec<&Task>> = Vec::new();
        let mut queue: VecDeque<usize> = VecDeque::new();
        for (i, &deg) in in_degree.iter().enumerate() {
            if deg == 0 {
                queue.push_back(i);
            }
        }

        while !queue.is_empty() {
            let level_size = queue.len();
            let mut level = Vec::new();
            for _ in 0..level_size {
                let node = queue.pop_front().expect("queue not empty");
                level.push(&self.tasks[node]);
                for &neighbor in &adj[node] {
                    in_degree[neighbor] -= 1;
                    if in_degree[neighbor] == 0 {
                        queue.push_back(neighbor);
                    }
                }
            }
            levels.push(level);
        }

        levels
    }

    /// Get a reference to the tasks in the graph.
    #[allow(dead_code)]
    pub fn tasks(&self) -> &[Task] {
        &self.tasks
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::TaskSize;

    fn make_task(id: &str, deps: Vec<&str>) -> Task {
        Task {
            id: id.to_string(),
            name: format!("Task {}", id),
            size: TaskSize::S,
            status: TaskStatus::Open,
            depends_on: deps.into_iter().map(String::from).collect(),
            done_when: "tests pass".to_string(),
            scope: "scope".to_string(),
            files_to_touch: vec![],
            not_to_change: vec![],
            branch: format!("task/{}-task-{}", id, id),
            interface: None,
        }
    }

    fn make_task_with_status(id: &str, deps: Vec<&str>, status: TaskStatus) -> Task {
        let mut t = make_task(id, deps);
        t.status = status;
        t
    }

    #[test]
    fn test_build_valid_graph() {
        let tasks = vec![make_task("001", vec![]), make_task("002", vec!["001"])];
        let graph = DependencyGraph::build(tasks);
        assert!(graph.is_ok());
    }

    #[test]
    fn test_build_invalid_dep() {
        let tasks = vec![make_task("001", vec!["999"])];
        let result = DependencyGraph::build(tasks);
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("non-existent"));
    }

    #[test]
    fn test_validate_no_cycle() {
        let tasks = vec![
            make_task("001", vec![]),
            make_task("002", vec!["001"]),
            make_task("003", vec!["001", "002"]),
        ];
        let graph = DependencyGraph::build(tasks).unwrap();
        assert!(graph.validate().is_ok());
    }

    #[test]
    fn test_validate_cycle() {
        // Create a cycle: 001 -> 002 -> 003 -> 001
        let tasks = vec![
            make_task("001", vec!["003"]),
            make_task("002", vec!["001"]),
            make_task("003", vec!["002"]),
        ];
        let graph = DependencyGraph::build(tasks).unwrap();
        let result = graph.validate();
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("cycle"));
    }

    #[test]
    fn test_validate_self_cycle() {
        let tasks = vec![make_task("001", vec!["001"])];
        let graph = DependencyGraph::build(tasks).unwrap();
        let result = graph.validate();
        assert!(result.is_err());
    }

    #[test]
    fn test_eligible_tasks_all_open_no_deps() {
        let tasks = vec![make_task("001", vec![]), make_task("002", vec![])];
        let graph = DependencyGraph::build(tasks).unwrap();
        let eligible = graph.eligible_tasks();
        assert_eq!(eligible.len(), 2);
    }

    #[test]
    fn test_eligible_tasks_with_deps() {
        let tasks = vec![
            make_task_with_status("001", vec![], TaskStatus::Done),
            make_task("002", vec!["001"]),
            make_task("003", vec!["002"]),
        ];
        let graph = DependencyGraph::build(tasks).unwrap();
        let eligible = graph.eligible_tasks();
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "002");
    }

    #[test]
    fn test_eligible_tasks_none_when_blocked() {
        let tasks = vec![make_task("001", vec![]), make_task("002", vec!["001"])];
        let graph = DependencyGraph::build(tasks).unwrap();
        let eligible = graph.eligible_tasks();
        // 001 is open, has no deps, so it's eligible
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "001");
    }

    #[test]
    fn test_parallel_safe_linear() {
        let tasks = vec![
            make_task("001", vec![]),
            make_task("002", vec!["001"]),
            make_task("003", vec!["002"]),
        ];
        let graph = DependencyGraph::build(tasks).unwrap();
        let levels = graph.parallel_safe();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].len(), 1);
        assert_eq!(levels[0][0].id, "001");
        assert_eq!(levels[1][0].id, "002");
        assert_eq!(levels[2][0].id, "003");
    }

    #[test]
    fn test_parallel_safe_diamond() {
        // 001 -> 002, 001 -> 003, 002+003 -> 004
        let tasks = vec![
            make_task("001", vec![]),
            make_task("002", vec!["001"]),
            make_task("003", vec!["001"]),
            make_task("004", vec!["002", "003"]),
        ];
        let graph = DependencyGraph::build(tasks).unwrap();
        let levels = graph.parallel_safe();
        assert_eq!(levels.len(), 3);
        assert_eq!(levels[0].len(), 1); // 001
        assert_eq!(levels[1].len(), 2); // 002, 003
        assert_eq!(levels[2].len(), 1); // 004
    }

    #[test]
    fn test_parallel_safe_independent() {
        let tasks = vec![
            make_task("001", vec![]),
            make_task("002", vec![]),
            make_task("003", vec![]),
        ];
        let graph = DependencyGraph::build(tasks).unwrap();
        let levels = graph.parallel_safe();
        assert_eq!(levels.len(), 1);
        assert_eq!(levels[0].len(), 3);
    }

    #[test]
    fn test_parallel_safe_empty() {
        let tasks: Vec<Task> = vec![];
        let graph = DependencyGraph::build(tasks).unwrap();
        let levels = graph.parallel_safe();
        assert!(levels.is_empty());
    }

    #[test]
    fn test_eligible_excludes_done_and_in_progress() {
        let tasks = vec![
            make_task_with_status("001", vec![], TaskStatus::Done),
            make_task_with_status("002", vec!["001"], TaskStatus::InProgress),
            make_task("003", vec!["001"]),
        ];
        let graph = DependencyGraph::build(tasks).unwrap();
        let eligible = graph.eligible_tasks();
        assert_eq!(eligible.len(), 1);
        assert_eq!(eligible[0].id, "003");
    }

    #[test]
    fn test_eligible_failed_not_eligible() {
        let tasks = vec![
            make_task_with_status("001", vec![], TaskStatus::Failed),
            make_task("002", vec!["001"]),
        ];
        let graph = DependencyGraph::build(tasks).unwrap();
        let eligible = graph.eligible_tasks();
        // 002 depends on 001 which is failed (not done), so 002 is not eligible
        // 001 is failed, not open, so 001 is not eligible either
        assert!(eligible.is_empty());
    }
}
