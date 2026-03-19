use anyhow::Result;

use crate::llm::LlmClient;
use crate::types::Finding;

/// Result of running all three parallel reviews.
#[derive(Debug, Clone)]
pub struct ReviewResult {
    pub code_review: Finding,
    pub qa_review: Finding,
    pub style_review: Finding,
}

const CODE_REVIEW_SYSTEM: &str = r#"You are a senior software engineer doing a code review. You receive a SPEC.md, a task definition, and a git diff. Evaluate:
- Does the implementation match the spec contract?
- Are there correctness bugs or security issues?
- Are error cases handled?
- Are all "done when" criteria met?

Output JSON only:
{"verdict":"pass|fail","critical":["..."],"warnings":["..."],"suggestions":["..."]}
Critical = must fix before merge. Warnings = should fix. Suggestions = optional."#;

const QA_REVIEW_SYSTEM: &str = r#"You are a QA engineer reviewing unit tests. You receive a SPEC.md, a task definition, and a git diff.

FIRST: determine whether this diff contains testable logic.
Scaffolding, boilerplate, and setup tasks do NOT require tests. Examples that do NOT need tests:
- Cargo.toml / dependency changes
- Empty struct / enum / module declarations with no logic
- Stub functions that only contain `todo!()`, `unimplemented!()`, or `Ok(())`
- Project layout setup (creating files, adding mod declarations)
- Config file changes

If the diff contains no testable logic, output: {"verdict":"pass","critical":[],"warnings":[],"suggestions":["Add tests when logic is implemented"]}

If the diff DOES contain real logic (non-trivial functions, business rules, algorithms, I/O handling), then evaluate:
- Do the tests actually test behaviour, not just lines?
- Are edge cases covered?
- Would any test pass if the implementation was subtly wrong?
- Is test coverage proportional to the complexity introduced?

Mark critical ONLY if real logic exists with zero tests. Do not require 90% coverage for tasks that are primarily setup or scaffolding.

Output JSON only:
{"verdict":"pass|fail","critical":["..."],"warnings":["..."],"suggestions":["..."]}
"#;

const STYLE_REVIEW_SYSTEM: &str = r#"You are a code quality reviewer. You receive a SPEC.md, a task definition, and a git diff. Evaluate:
- Is the code unnecessarily complex?
- Are there simpler ways to express the same logic?
- Are names clear and consistent?
- Are there any obvious refactoring opportunities?

IMPORTANT: Style issues almost never warrant a "fail" verdict.
Only mark verdict "fail" with a critical item if the code has a SEVERE structural problem — e.g. spaghetti logic that makes the module impossible to maintain, completely wrong abstraction that violates the spec contract, or names so misleading they would cause bugs.

Things that are NOT critical (put in warnings or suggestions only):
- Naming preferences
- Minor simplification opportunities
- Comment style
- Code organisation preferences
- Missing docs on internal functions
- Rustfmt/clippy style nits

When in doubt, use verdict "pass" with warnings or suggestions. Reserve "fail" for genuinely unreadable or dangerously misleading code.

Output JSON only:
{"verdict":"pass|fail","critical":["..."],"warnings":["..."],"suggestions":["..."]}"#;

/// Build the user prompt containing spec, task detail, branch context, and diff.
fn build_review_user_prompt(
    spec_content: &str,
    task_detail: &str,
    diff: &str,
    commit_log: &str,
    changed_files: &str,
) -> String {
    let mut prompt = format!(
        "SPEC.md:\n{}\n\nTask details:\n{}",
        spec_content, task_detail
    );

    if !commit_log.is_empty() {
        prompt.push_str(&format!("\n\nCommits on this branch:\n{}", commit_log));
    }

    if !changed_files.is_empty() {
        prompt.push_str(&format!(
            "\n\nAll files changed on this branch:\n{}",
            changed_files
        ));
    }

    prompt.push_str(&format!("\n\nGit diff (most recent changes):\n{}", diff));
    prompt
}

/// Parse a JSON review response into a Finding.
pub fn parse_finding(response: &str) -> Finding {
    // Try to extract JSON from the response (may have markdown wrapping)
    let json_str = extract_json(response);

    match serde_json::from_str::<Finding>(json_str) {
        Ok(finding) => finding,
        Err(_) => Finding::invalid_json("Review agent returned invalid JSON"),
    }
}

/// Extract JSON from a response that may have markdown code fences.
fn extract_json(response: &str) -> &str {
    let trimmed = response.trim();

    // Try to find JSON block in code fences
    if let Some(start) = trimmed.find("```json") {
        let after_fence = &trimmed[start + 7..];
        if let Some(end) = after_fence.find("```") {
            return after_fence[..end].trim();
        }
    }

    if let Some(start) = trimmed.find("```") {
        let after_fence = &trimmed[start + 3..];
        if let Some(end) = after_fence.find("```") {
            return after_fence[..end].trim();
        }
    }

    // Try raw JSON (starts with {)
    if let Some(start) = trimmed.find('{') {
        if let Some(end) = trimmed.rfind('}') {
            return &trimmed[start..=end];
        }
    }

    trimmed
}

/// Run all three reviews concurrently, with one automatic retry on invalid JSON.
#[allow(clippy::too_many_arguments)]
pub async fn run_reviews(
    // workdir added
    llm: &dyn LlmClient,
    spec_content: &str,
    task_detail: &str,
    diff: &str,
    commit_log: &str,
    changed_files: &str,
    workdir: &std::path::Path,
) -> Result<ReviewResult> {
    use crate::instructions::{as_system_appendix, load, AgentKind};

    // Build per-reviewer system prompts with optional project instructions appended
    let code_sys = format!(
        "{CODE_REVIEW_SYSTEM}{}",
        as_system_appendix(&load(workdir, AgentKind::CodeReviewer))
    );
    let qa_sys = format!(
        "{QA_REVIEW_SYSTEM}{}",
        as_system_appendix(&load(workdir, AgentKind::QaReviewer))
    );
    let style_sys = format!(
        "{STYLE_REVIEW_SYSTEM}{}",
        as_system_appendix(&load(workdir, AgentKind::StyleReviewer))
    );

    let user_prompt =
        build_review_user_prompt(spec_content, task_detail, diff, commit_log, changed_files);

    let (code_resp, qa_resp, style_resp) = tokio::try_join!(
        llm.complete(&code_sys, &user_prompt),
        llm.complete(&qa_sys, &user_prompt),
        llm.complete(&style_sys, &user_prompt),
    )?;

    let code_review = parse_finding(&code_resp);
    let qa_review = parse_finding(&qa_resp);
    let mut style_review = parse_finding(&style_resp);

    // Retry style review if it returned invalid JSON — style LLM occasionally
    // wraps its response in prose instead of raw JSON
    if style_review
        .critical
        .iter()
        .any(|c| c.contains("invalid JSON"))
    {
        if let Ok(retry_resp) = llm.complete(&style_sys, &user_prompt).await {
            let retry = parse_finding(&retry_resp);
            if !retry.critical.iter().any(|c| c.contains("invalid JSON")) {
                style_review = retry;
            }
        }
    }

    Ok(ReviewResult {
        code_review,
        qa_review,
        style_review,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::Verdict;

    #[test]
    fn test_parse_finding_valid_pass() {
        let json = r#"{"verdict":"pass","critical":[],"warnings":["minor thing"],"suggestions":["use const"]}"#;
        let finding = parse_finding(json);
        assert_eq!(finding.verdict, Verdict::Pass);
        assert!(finding.critical.is_empty());
        assert_eq!(finding.warnings.len(), 1);
        assert_eq!(finding.suggestions.len(), 1);
    }

    #[test]
    fn test_parse_finding_valid_fail() {
        let json = r#"{"verdict":"fail","critical":["missing error handling"],"warnings":[],"suggestions":[]}"#;
        let finding = parse_finding(json);
        assert_eq!(finding.verdict, Verdict::Fail);
        assert_eq!(finding.critical.len(), 1);
        assert_eq!(finding.critical[0], "missing error handling");
    }

    #[test]
    fn test_parse_finding_invalid_json() {
        let finding = parse_finding("not json at all");
        assert_eq!(finding.verdict, Verdict::Fail);
        assert_eq!(finding.critical.len(), 1);
        assert!(finding.critical[0].contains("invalid JSON"));
    }

    #[test]
    fn test_parse_finding_empty_string() {
        let finding = parse_finding("");
        assert_eq!(finding.verdict, Verdict::Fail);
    }

    #[test]
    fn test_parse_finding_with_code_fence() {
        let response = "Here is my review:\n```json\n{\"verdict\":\"pass\",\"critical\":[],\"warnings\":[],\"suggestions\":[]}\n```";
        let finding = parse_finding(response);
        assert_eq!(finding.verdict, Verdict::Pass);
    }

    #[test]
    fn test_parse_finding_with_generic_fence() {
        let response = "```\n{\"verdict\":\"fail\",\"critical\":[\"bug\"],\"warnings\":[],\"suggestions\":[]}\n```";
        let finding = parse_finding(response);
        assert_eq!(finding.verdict, Verdict::Fail);
        assert_eq!(finding.critical[0], "bug");
    }

    #[test]
    fn test_parse_finding_json_with_surrounding_text() {
        let response = "My analysis:\n{\"verdict\":\"pass\",\"critical\":[],\"warnings\":[],\"suggestions\":[]}\nEnd of review.";
        let finding = parse_finding(response);
        assert_eq!(finding.verdict, Verdict::Pass);
    }

    #[test]
    fn test_build_review_user_prompt() {
        let prompt =
            build_review_user_prompt("spec", "task", "diff content", "abc fix", "A src/lib.rs");
        assert!(prompt.contains("SPEC.md:\nspec"));
        assert!(prompt.contains("Task details:\ntask"));
        assert!(prompt.contains("Git diff (most recent changes):\ndiff content"));
        assert!(prompt.contains("Commits on this branch:\nabc fix"));
        assert!(prompt.contains("All files changed on this branch:\nA src/lib.rs"));
    }

    #[test]
    fn test_extract_json_raw() {
        let result = extract_json(r#"{"verdict":"pass"}"#);
        assert!(result.contains("verdict"));
    }

    #[test]
    fn test_extract_json_fenced() {
        let result = extract_json("```json\n{\"verdict\":\"pass\"}\n```");
        assert!(result.contains("verdict"));
    }

    #[test]
    fn test_system_prompts_contain_json_format() {
        assert!(CODE_REVIEW_SYSTEM.contains("verdict"));
        assert!(QA_REVIEW_SYSTEM.contains("verdict"));
        assert!(STYLE_REVIEW_SYSTEM.contains("verdict"));
    }

    #[tokio::test]
    async fn test_run_reviews_with_mock() {
        use async_trait::async_trait;

        struct MockLlm;

        #[async_trait]
        impl LlmClient for MockLlm {
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                Ok(
                    r#"{"verdict":"pass","critical":[],"warnings":[],"suggestions":[]}"#
                        .to_string(),
                )
            }
        }

        let llm = MockLlm;
        let result = run_reviews(
            &llm,
            "spec",
            "task",
            "diff",
            "abc123 add builder",
            "A src/builder.rs",
            std::path::Path::new("/tmp"),
        )
        .await
        .unwrap();
        assert_eq!(result.code_review.verdict, Verdict::Pass);
        assert_eq!(result.qa_review.verdict, Verdict::Pass);
        assert_eq!(result.style_review.verdict, Verdict::Pass);
    }

    #[tokio::test]
    async fn test_run_reviews_mixed_results() {
        use async_trait::async_trait;
        use std::sync::atomic::{AtomicU32, Ordering};

        struct MockLlm {
            call_count: AtomicU32,
        }

        #[async_trait]
        impl LlmClient for MockLlm {
            async fn complete(&self, system: &str, _user: &str) -> Result<String> {
                let _ = self.call_count.fetch_add(1, Ordering::SeqCst);
                if system.contains("senior software engineer") {
                    Ok(r#"{"verdict":"fail","critical":["bug found"],"warnings":[],"suggestions":[]}"#.to_string())
                } else {
                    Ok(
                        r#"{"verdict":"pass","critical":[],"warnings":[],"suggestions":[]}"#
                            .to_string(),
                    )
                }
            }
        }

        let llm = MockLlm {
            call_count: AtomicU32::new(0),
        };
        let result = run_reviews(
            &llm,
            "spec",
            "task",
            "diff",
            "abc123 add builder",
            "A src/builder.rs",
            std::path::Path::new("/tmp"),
        )
        .await
        .unwrap();
        assert_eq!(result.code_review.verdict, Verdict::Fail);
        assert_eq!(result.qa_review.verdict, Verdict::Pass);
        assert_eq!(result.style_review.verdict, Verdict::Pass);
    }

    #[tokio::test]
    async fn test_run_reviews_llm_error() {
        use async_trait::async_trait;

        struct FailingLlm;

        #[async_trait]
        impl LlmClient for FailingLlm {
            async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
                anyhow::bail!("API error")
            }
        }

        let llm = FailingLlm;
        let result = run_reviews(
            &llm,
            "spec",
            "task",
            "diff",
            "abc123 add builder",
            "A src/builder.rs",
            std::path::Path::new("/tmp"),
        )
        .await;
        assert!(result.is_err());
    }
}
