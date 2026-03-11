#![allow(dead_code)]

use serde::{Deserialize, Serialize};
use std::fmt;

/// A single question-answer pair from the Q&A session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct QaPair {
    pub question: String,
    pub answer: Option<String>,
}

/// Task size: S (<2h), M (~half day), L (>half day, must be split).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum TaskSize {
    S,
    M,
    L,
}

impl fmt::Display for TaskSize {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskSize::S => write!(f, "S"),
            TaskSize::M => write!(f, "M"),
            TaskSize::L => write!(f, "L"),
        }
    }
}

impl std::str::FromStr for TaskSize {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_uppercase().as_str() {
            "S" => Ok(TaskSize::S),
            "M" => Ok(TaskSize::M),
            "L" => Ok(TaskSize::L),
            other => Err(anyhow::anyhow!("Unknown task size: {other}. Use S, M, or L.")),
        }
    }
}

/// Task status for tracking progress.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum TaskStatus {
    Open,
    InProgress,
    Done,
    Failed,
}

impl fmt::Display for TaskStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            TaskStatus::Open => write!(f, "open"),
            TaskStatus::InProgress => write!(f, "in-progress"),
            TaskStatus::Done => write!(f, "done"),
            TaskStatus::Failed => write!(f, "failed"),
        }
    }
}

impl std::str::FromStr for TaskStatus {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "open" => Ok(TaskStatus::Open),
            "in-progress" | "in_progress" => Ok(TaskStatus::InProgress),
            "done" => Ok(TaskStatus::Done),
            "failed" => Ok(TaskStatus::Failed),
            other => Err(anyhow::anyhow!("Unknown task status: {other}")),
        }
    }
}

/// A single task in the project plan.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub name: String,
    pub size: TaskSize,
    pub status: TaskStatus,
    pub depends_on: Vec<String>,
    pub done_when: String,
    pub scope: String,
    pub files_to_touch: Vec<String>,
    pub not_to_change: Vec<String>,
    pub branch: String,
    pub interface: Option<String>,
}

impl Task {
    /// Generate the conventional branch name for this task.
    pub fn default_branch(id: &str, name: &str) -> String {
        let slug: String = name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        format!("task/{}-{}", id, slug)
    }

    /// Whether this task has a detail file (M and L tasks).
    pub fn has_detail_file(&self) -> bool {
        matches!(self.size, TaskSize::M | TaskSize::L)
    }

    /// Generate the detail file path relative to project root.
    pub fn detail_filename(&self) -> String {
        let slug: String = self
            .name
            .to_lowercase()
            .chars()
            .map(|c| if c.is_alphanumeric() { c } else { '-' })
            .collect::<String>()
            .split('-')
            .filter(|s| !s.is_empty())
            .collect::<Vec<_>>()
            .join("-");
        format!("tasks/{}-{}.md", self.id, slug)
    }
}

impl QaPair {
    pub fn answered(&self) -> bool {
        self.answer
            .as_ref()
            .map(|a| !a.trim().is_empty())
            .unwrap_or(false)
    }
}

/// Review verdict from an LLM review agent.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Verdict {
    Pass,
    Fail,
}

impl fmt::Display for Verdict {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Verdict::Pass => write!(f, "pass"),
            Verdict::Fail => write!(f, "fail"),
        }
    }
}

impl std::str::FromStr for Verdict {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_lowercase().as_str() {
            "pass" => Ok(Verdict::Pass),
            "fail" => Ok(Verdict::Fail),
            other => Err(anyhow::anyhow!("Unknown verdict: {other}. Use 'pass' or 'fail'.")),
        }
    }
}

/// A single review finding from an LLM review agent.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    pub verdict: Verdict,
    pub critical: Vec<String>,
    pub warnings: Vec<String>,
    pub suggestions: Vec<String>,
}

impl Finding {
    pub fn passed(&self) -> bool {
        self.verdict == Verdict::Pass
    }

    /// Create a failure finding for invalid JSON responses.
    pub fn invalid_json(message: &str) -> Self {
        Finding {
            verdict: Verdict::Fail,
            critical: vec![message.to_string()],
            warnings: vec![],
            suggestions: vec![],
        }
    }
}

/// Represents the LLM provider choice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Provider {
    Anthropic,
    OpenAI,
}

impl std::fmt::Display for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Provider::Anthropic => write!(f, "anthropic"),
            Provider::OpenAI => write!(f, "openai"),
        }
    }
}

impl std::str::FromStr for Provider {
    type Err = anyhow::Error;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.to_lowercase().as_str() {
            "anthropic" => Ok(Provider::Anthropic),
            "openai" => Ok(Provider::OpenAI),
            other => Err(anyhow::anyhow!("Unknown provider: {other}. Use 'anthropic' or 'openai'.")),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Task type tests ---

    #[test]
    fn test_task_size_display() {
        assert_eq!(TaskSize::S.to_string(), "S");
        assert_eq!(TaskSize::M.to_string(), "M");
        assert_eq!(TaskSize::L.to_string(), "L");
    }

    #[test]
    fn test_task_size_from_str() {
        assert_eq!("S".parse::<TaskSize>().unwrap(), TaskSize::S);
        assert_eq!("m".parse::<TaskSize>().unwrap(), TaskSize::M);
        assert_eq!(" L ".parse::<TaskSize>().unwrap(), TaskSize::L);
        assert!("X".parse::<TaskSize>().is_err());
    }

    #[test]
    fn test_task_status_display() {
        assert_eq!(TaskStatus::Open.to_string(), "open");
        assert_eq!(TaskStatus::InProgress.to_string(), "in-progress");
        assert_eq!(TaskStatus::Done.to_string(), "done");
        assert_eq!(TaskStatus::Failed.to_string(), "failed");
    }

    #[test]
    fn test_task_status_from_str() {
        assert_eq!("open".parse::<TaskStatus>().unwrap(), TaskStatus::Open);
        assert_eq!("in-progress".parse::<TaskStatus>().unwrap(), TaskStatus::InProgress);
        assert_eq!("in_progress".parse::<TaskStatus>().unwrap(), TaskStatus::InProgress);
        assert_eq!("done".parse::<TaskStatus>().unwrap(), TaskStatus::Done);
        assert_eq!("failed".parse::<TaskStatus>().unwrap(), TaskStatus::Failed);
        assert!("unknown".parse::<TaskStatus>().is_err());
    }

    #[test]
    fn test_task_default_branch() {
        assert_eq!(
            Task::default_branch("001", "Scaffold project structure"),
            "task/001-scaffold-project-structure"
        );
        assert_eq!(
            Task::default_branch("002", "Implement data models"),
            "task/002-implement-data-models"
        );
    }

    #[test]
    fn test_task_has_detail_file() {
        let make_task = |size: TaskSize| Task {
            id: "001".to_string(),
            name: "Test".to_string(),
            size,
            status: TaskStatus::Open,
            depends_on: vec![],
            done_when: "tests pass".to_string(),
            scope: "scope".to_string(),
            files_to_touch: vec![],
            not_to_change: vec![],
            branch: "task/001-test".to_string(),
            interface: None,
        };
        assert!(!make_task(TaskSize::S).has_detail_file());
        assert!(make_task(TaskSize::M).has_detail_file());
        assert!(make_task(TaskSize::L).has_detail_file());
    }

    #[test]
    fn test_task_detail_filename() {
        let task = Task {
            id: "002".to_string(),
            name: "Implement data models".to_string(),
            size: TaskSize::M,
            status: TaskStatus::Open,
            depends_on: vec![],
            done_when: "tests pass".to_string(),
            scope: "scope".to_string(),
            files_to_touch: vec![],
            not_to_change: vec![],
            branch: "task/002-implement-data-models".to_string(),
            interface: None,
        };
        assert_eq!(task.detail_filename(), "tasks/002-implement-data-models.md");
    }

    #[test]
    fn test_task_serialization_roundtrip() {
        let task = Task {
            id: "001".to_string(),
            name: "Scaffold".to_string(),
            size: TaskSize::S,
            status: TaskStatus::Open,
            depends_on: vec![],
            done_when: "cargo build passes".to_string(),
            scope: "Create project".to_string(),
            files_to_touch: vec!["Cargo.toml".to_string()],
            not_to_change: vec![],
            branch: "task/001-scaffold".to_string(),
            interface: None,
        };
        let json = serde_json::to_string(&task).unwrap();
        let deserialized: Task = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.id, "001");
        assert_eq!(deserialized.size, TaskSize::S);
        assert_eq!(deserialized.status, TaskStatus::Open);
    }

    // --- Verdict tests ---

    #[test]
    fn test_verdict_display() {
        assert_eq!(Verdict::Pass.to_string(), "pass");
        assert_eq!(Verdict::Fail.to_string(), "fail");
    }

    #[test]
    fn test_verdict_from_str() {
        assert_eq!("pass".parse::<Verdict>().unwrap(), Verdict::Pass);
        assert_eq!("fail".parse::<Verdict>().unwrap(), Verdict::Fail);
        assert_eq!("PASS".parse::<Verdict>().unwrap(), Verdict::Pass);
        assert!("unknown".parse::<Verdict>().is_err());
    }

    #[test]
    fn test_finding_passed() {
        let pass = Finding {
            verdict: Verdict::Pass,
            critical: vec![],
            warnings: vec![],
            suggestions: vec![],
        };
        assert!(pass.passed());

        let fail = Finding {
            verdict: Verdict::Fail,
            critical: vec!["bug".to_string()],
            warnings: vec![],
            suggestions: vec![],
        };
        assert!(!fail.passed());
    }

    #[test]
    fn test_finding_invalid_json() {
        let f = Finding::invalid_json("bad response");
        assert_eq!(f.verdict, Verdict::Fail);
        assert_eq!(f.critical.len(), 1);
        assert!(f.critical[0].contains("bad response"));
        assert!(f.warnings.is_empty());
        assert!(f.suggestions.is_empty());
    }

    #[test]
    fn test_finding_serialization_roundtrip() {
        let finding = Finding {
            verdict: Verdict::Pass,
            critical: vec![],
            warnings: vec!["minor".to_string()],
            suggestions: vec!["hint".to_string()],
        };
        let json = serde_json::to_string(&finding).unwrap();
        let deserialized: Finding = serde_json::from_str(&json).unwrap();
        assert_eq!(deserialized.verdict, Verdict::Pass);
        assert_eq!(deserialized.warnings.len(), 1);
    }

    // --- QA pair tests ---

    #[test]
    fn test_qa_pair_answered() {
        let pair = QaPair {
            question: "What?".to_string(),
            answer: Some("Something".to_string()),
        };
        assert!(pair.answered());
    }

    #[test]
    fn test_qa_pair_unanswered_none() {
        let pair = QaPair {
            question: "What?".to_string(),
            answer: None,
        };
        assert!(!pair.answered());
    }

    #[test]
    fn test_qa_pair_unanswered_empty() {
        let pair = QaPair {
            question: "What?".to_string(),
            answer: Some("".to_string()),
        };
        assert!(!pair.answered());
    }

    #[test]
    fn test_qa_pair_unanswered_whitespace() {
        let pair = QaPair {
            question: "What?".to_string(),
            answer: Some("   ".to_string()),
        };
        assert!(!pair.answered());
    }

    #[test]
    fn test_provider_display() {
        assert_eq!(Provider::Anthropic.to_string(), "anthropic");
        assert_eq!(Provider::OpenAI.to_string(), "openai");
    }

    #[test]
    fn test_provider_from_str() {
        assert_eq!("anthropic".parse::<Provider>().unwrap(), Provider::Anthropic);
        assert_eq!("openai".parse::<Provider>().unwrap(), Provider::OpenAI);
        assert_eq!("Anthropic".parse::<Provider>().unwrap(), Provider::Anthropic);
        assert!("unknown".parse::<Provider>().is_err());
    }
}
