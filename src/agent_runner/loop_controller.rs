use crate::agent_runner::review::ReviewResult;
use crate::types::Verdict;

/// Error returned when the loop iteration limit is exceeded.
#[derive(Debug, thiserror::Error)]
#[error("Loop limit exceeded: {current} > {max}")]
pub struct LoopLimitError {
    pub current: u32,
    pub max: u32,
}

/// Controls the review loop iteration count and pass/fail logic.
pub struct LoopController {
    max_iterations: u32,
    current: u32,
}

impl LoopController {
    pub fn new(max: u32) -> Self {
        LoopController {
            max_iterations: max,
            current: 0,
        }
    }

    /// Increment the iteration counter. Returns Err if limit exceeded.
    pub fn increment(&mut self) -> Result<(), LoopLimitError> {
        self.current += 1;
        if self.current > self.max_iterations {
            Err(LoopLimitError {
                current: self.current,
                max: self.max_iterations,
            })
        } else {
            Ok(())
        }
    }

    /// Returns the current iteration number.
    pub fn iteration(&self) -> u32 {
        self.current
    }

    /// Returns true if all three reviews passed.
    pub fn all_passed(result: &ReviewResult) -> bool {
        result.code_review.verdict == Verdict::Pass
            && result.qa_review.verdict == Verdict::Pass
            && result.style_review.verdict == Verdict::Pass
    }

    /// Merges all findings into a single prompt for the coding agent.
    pub fn findings_prompt(result: &ReviewResult) -> String {
        let mut prompt = String::new();

        Self::append_findings(&mut prompt, "Code Review", &result.code_review);
        Self::append_findings(&mut prompt, "QA Review", &result.qa_review);
        Self::append_findings(&mut prompt, "Style Review", &result.style_review);

        prompt
    }

    fn append_findings(
        prompt: &mut String,
        label: &str,
        finding: &crate::types::Finding,
    ) {
        prompt.push_str(&format!("## {} ({})\n", label, finding.verdict));

        if !finding.critical.is_empty() {
            prompt.push_str("### Critical (must fix):\n");
            for item in &finding.critical {
                prompt.push_str(&format!("- {}\n", item));
            }
        }

        if !finding.warnings.is_empty() {
            prompt.push_str("### Warnings:\n");
            for item in &finding.warnings {
                prompt.push_str(&format!("- {}\n", item));
            }
        }

        if !finding.suggestions.is_empty() {
            prompt.push_str("### Suggestions:\n");
            for item in &finding.suggestions {
                prompt.push_str(&format!("- {}\n", item));
            }
        }

        prompt.push('\n');
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Finding, Verdict};

    fn pass_finding() -> Finding {
        Finding {
            verdict: Verdict::Pass,
            critical: vec![],
            warnings: vec![],
            suggestions: vec![],
        }
    }

    fn fail_finding() -> Finding {
        Finding {
            verdict: Verdict::Fail,
            critical: vec!["missing error handling".to_string()],
            warnings: vec!["unused import".to_string()],
            suggestions: vec!["use const".to_string()],
        }
    }

    fn pass_review() -> ReviewResult {
        ReviewResult {
            code_review: pass_finding(),
            qa_review: pass_finding(),
            style_review: pass_finding(),
        }
    }

    fn mixed_review() -> ReviewResult {
        ReviewResult {
            code_review: fail_finding(),
            qa_review: pass_finding(),
            style_review: pass_finding(),
        }
    }

    #[test]
    fn test_new_controller() {
        let ctrl = LoopController::new(5);
        assert_eq!(ctrl.iteration(), 0);
    }

    #[test]
    fn test_increment_within_limit() {
        let mut ctrl = LoopController::new(3);
        assert!(ctrl.increment().is_ok());
        assert_eq!(ctrl.iteration(), 1);
        assert!(ctrl.increment().is_ok());
        assert_eq!(ctrl.iteration(), 2);
        assert!(ctrl.increment().is_ok());
        assert_eq!(ctrl.iteration(), 3);
    }

    #[test]
    fn test_increment_exceeds_limit() {
        let mut ctrl = LoopController::new(2);
        assert!(ctrl.increment().is_ok()); // 1
        assert!(ctrl.increment().is_ok()); // 2
        let err = ctrl.increment().unwrap_err(); // 3 > 2
        assert_eq!(err.current, 3);
        assert_eq!(err.max, 2);
    }

    #[test]
    fn test_increment_limit_one() {
        let mut ctrl = LoopController::new(1);
        assert!(ctrl.increment().is_ok()); // 1
        assert!(ctrl.increment().is_err()); // 2 > 1
    }

    #[test]
    fn test_all_passed_true() {
        assert!(LoopController::all_passed(&pass_review()));
    }

    #[test]
    fn test_all_passed_false_code() {
        assert!(!LoopController::all_passed(&mixed_review()));
    }

    #[test]
    fn test_all_passed_false_qa() {
        let result = ReviewResult {
            code_review: pass_finding(),
            qa_review: fail_finding(),
            style_review: pass_finding(),
        };
        assert!(!LoopController::all_passed(&result));
    }

    #[test]
    fn test_all_passed_false_style() {
        let result = ReviewResult {
            code_review: pass_finding(),
            qa_review: pass_finding(),
            style_review: fail_finding(),
        };
        assert!(!LoopController::all_passed(&result));
    }

    #[test]
    fn test_all_passed_all_fail() {
        let result = ReviewResult {
            code_review: fail_finding(),
            qa_review: fail_finding(),
            style_review: fail_finding(),
        };
        assert!(!LoopController::all_passed(&result));
    }

    #[test]
    fn test_findings_prompt_all_pass() {
        let prompt = LoopController::findings_prompt(&pass_review());
        assert!(prompt.contains("Code Review (pass)"));
        assert!(prompt.contains("QA Review (pass)"));
        assert!(prompt.contains("Style Review (pass)"));
        assert!(!prompt.contains("Critical"));
    }

    #[test]
    fn test_findings_prompt_with_failures() {
        let prompt = LoopController::findings_prompt(&mixed_review());
        assert!(prompt.contains("Code Review (fail)"));
        assert!(prompt.contains("### Critical (must fix):"));
        assert!(prompt.contains("- missing error handling"));
        assert!(prompt.contains("### Warnings:"));
        assert!(prompt.contains("- unused import"));
        assert!(prompt.contains("### Suggestions:"));
        assert!(prompt.contains("- use const"));
        assert!(prompt.contains("QA Review (pass)"));
    }

    #[test]
    fn test_findings_prompt_format() {
        let result = ReviewResult {
            code_review: Finding {
                verdict: Verdict::Fail,
                critical: vec!["bug A".to_string(), "bug B".to_string()],
                warnings: vec![],
                suggestions: vec![],
            },
            qa_review: pass_finding(),
            style_review: pass_finding(),
        };
        let prompt = LoopController::findings_prompt(&result);
        assert!(prompt.contains("- bug A"));
        assert!(prompt.contains("- bug B"));
    }

    #[test]
    fn test_loop_limit_error_display() {
        let err = LoopLimitError {
            current: 6,
            max: 5,
        };
        assert_eq!(err.to_string(), "Loop limit exceeded: 6 > 5");
    }
}
