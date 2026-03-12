/// The SPEC.md template that the LLM fills in.
pub const SPEC_TEMPLATE: &str = r#"---
spec-version: 1
created: {date}
updated: {date}
---

# Project: {name}

## Goal
{goal}

## Scope
- In scope: {in_scope}
- Out of scope: {out_of_scope}

## Stack
- Language/runtime: {language}
- Framework: {framework}
- Database: {database}
- Deployment: local script/program (Dockerfile on request)

## Data Models
{data_models}

## API / Interface Contracts
{api_contracts}

## Key Workflows
{workflows}

## Acceptance Criteria
{acceptance_criteria}

## Constraints & Non-Negotiables
- Unit test coverage: 90% minimum
{constraints}

## Open Questions
{open_questions}
"#;

/// Build the user prompt for the LLM: initial idea + Q&A pairs + template.
pub fn build_user_prompt(idea: &str, qa_pairs: &[(String, Option<String>)], today: &str) -> String {
    let mut prompt = format!("## Project Idea\n{}\n\n## Clarifying Q&A\n", idea);

    for (question, answer) in qa_pairs {
        let answer_text = answer
            .as_ref()
            .filter(|a| !a.trim().is_empty())
            .map(|a| a.as_str())
            .unwrap_or("(unanswered)");
        prompt.push_str(&format!("Q: {}\nA: {}\n\n", question, answer_text));
    }

    prompt.push_str("## Output Template\nFill in this template. Replace all {placeholders} with concrete content. Output ONLY the filled-in markdown.\n\n");

    // Include the template with today's date filled in
    let template = SPEC_TEMPLATE.replace("{date}", today);
    prompt.push_str(&template);

    prompt
}

/// Known section headings in SPEC.md for the "edit <section>" flow.
#[allow(dead_code)]
pub const SECTION_HEADINGS: &[&str] = &[
    "Goal",
    "Scope",
    "Stack",
    "Data Models",
    "API / Interface Contracts",
    "Key Workflows",
    "Acceptance Criteria",
    "Constraints & Non-Negotiables",
    "Open Questions",
];

/// Replace the content of a named section in the spec.
/// A section starts with "## <heading>" and ends before the next "## " or end of file.
#[allow(dead_code)]
pub fn replace_section(spec: &str, section_name: &str, new_content: &str) -> String {
    let target = format!("## {}", section_name);
    let mut result = String::with_capacity(spec.len());
    let mut lines = spec.lines().peekable();
    let mut found = false;

    while let Some(line) = lines.next() {
        if line.starts_with(&target) && !found {
            found = true;
            result.push_str(line);
            result.push('\n');
            // Skip old content until next section heading or end
            while let Some(next_line) = lines.peek() {
                if next_line.starts_with("## ") {
                    break;
                }
                lines.next();
            }
            // Insert new content
            result.push_str(new_content.trim());
            result.push_str("\n\n");
        } else {
            result.push_str(line);
            result.push('\n');
        }
    }

    // Remove trailing extra newline if needed
    while result.ends_with("\n\n\n") {
        result.pop();
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_spec_template_contains_placeholders() {
        assert!(SPEC_TEMPLATE.contains("{name}"));
        assert!(SPEC_TEMPLATE.contains("{goal}"));
        assert!(SPEC_TEMPLATE.contains("{date}"));
        assert!(SPEC_TEMPLATE.contains("{in_scope}"));
        assert!(SPEC_TEMPLATE.contains("{out_of_scope}"));
    }

    #[test]
    fn test_spec_template_has_all_sections() {
        for heading in SECTION_HEADINGS {
            let section_marker = format!("## {}", heading);
            assert!(
                SPEC_TEMPLATE.contains(&section_marker),
                "Template missing section: {}",
                heading
            );
        }
    }

    #[test]
    fn test_build_user_prompt_basic() {
        let qa = vec![
            (
                "What does this do?".to_string(),
                Some("A todo app".to_string()),
            ),
            ("Out of scope?".to_string(), None),
        ];
        let prompt = build_user_prompt("Build a todo app", &qa, "2024-01-15");
        assert!(prompt.contains("Build a todo app"));
        assert!(prompt.contains("A todo app"));
        assert!(prompt.contains("(unanswered)"));
        assert!(prompt.contains("2024-01-15"));
    }

    #[test]
    fn test_build_user_prompt_empty_answer_treated_as_unanswered() {
        let qa = vec![("Q1?".to_string(), Some("  ".to_string()))];
        let prompt = build_user_prompt("idea", &qa, "2024-01-01");
        assert!(prompt.contains("(unanswered)"));
    }

    #[test]
    fn test_section_headings_count() {
        assert_eq!(SECTION_HEADINGS.len(), 9);
    }

    #[test]
    fn test_replace_section_middle() {
        let spec = "## Goal\nOld goal content\n\n## Scope\nOld scope\n\n## Stack\nOld stack\n";
        let result = replace_section(spec, "Scope", "New scope content here");
        assert!(result.contains("## Scope\nNew scope content here"));
        assert!(result.contains("## Goal\nOld goal content"));
        assert!(result.contains("## Stack\nOld stack"));
        assert!(!result.contains("Old scope"));
    }

    #[test]
    fn test_replace_section_first() {
        let spec = "## Goal\nOld goal\n\n## Scope\nScope stuff\n";
        let result = replace_section(spec, "Goal", "New goal");
        assert!(result.contains("## Goal\nNew goal"));
        assert!(!result.contains("Old goal"));
        assert!(result.contains("## Scope\nScope stuff"));
    }

    #[test]
    fn test_replace_section_last() {
        let spec = "## Goal\nGoal stuff\n\n## Open Questions\nOld questions\n";
        let result = replace_section(spec, "Open Questions", "None at this time");
        assert!(result.contains("## Open Questions\nNone at this time"));
        assert!(!result.contains("Old questions"));
    }

    #[test]
    fn test_replace_section_not_found() {
        let spec = "## Goal\nGoal stuff\n";
        let result = replace_section(spec, "Nonexistent", "New content");
        // Should be unchanged
        assert!(result.contains("## Goal\nGoal stuff"));
    }
}
