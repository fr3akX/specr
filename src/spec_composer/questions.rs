use crate::llm::LlmClient;

const SYSTEM_PROMPT: &str = "\
You are a senior software architect. Given a project idea, generate targeted \
clarifying questions that would meaningfully improve the resulting project spec.\n\
\n\
Rules:\n\
- Ask only about things genuinely ambiguous for THIS project that would affect design decisions\n\
- No generic boilerplate filler\n\
- Each question must be specific and actionable\n\
- Aim for up to {budget} questions, but generate as many as the project genuinely needs\n\
- Output ONLY a JSON array of strings, no commentary, no markdown fences\n\
\n\
Example output:\n\
[\"What data should be tracked per workout?\", \"Should workouts be stored locally or in a database?\"]";

/// Fallback questions used if the LLM call fails or returns unparseable output.
const FALLBACK_QUESTIONS: &[&str] = &[
    "What is the primary goal of this project?",
    "What is explicitly out of scope for the first version?",
    "What language, runtime, and framework should be used?",
];

/// Ask the LLM to generate clarifying questions specific to the project idea.
/// Returns all questions the LLM produces — no truncation.
/// Falls back to a minimal baseline set if the LLM call fails or returns invalid JSON.
pub async fn generate(idea: &str, budget: usize, client: &dyn LlmClient) -> Vec<String> {
    let system = SYSTEM_PROMPT.replace("{budget}", &budget.to_string());
    let user = format!("Project idea: {}", idea);

    match client.complete(&system, &user).await {
        Ok(raw) => parse_questions(&raw),
        Err(_) => FALLBACK_QUESTIONS.iter().map(|s| s.to_string()).collect(),
    }
}

/// Parse the LLM's JSON array response into a Vec<String>.
/// Returns fallback questions if parsing fails.
fn parse_questions(raw: &str) -> Vec<String> {
    // Strip markdown fences if the LLM included them despite instructions
    let cleaned = raw
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();

    match serde_json::from_str::<Vec<String>>(cleaned) {
        Ok(questions) if !questions.is_empty() => questions,
        _ => FALLBACK_QUESTIONS.iter().map(|s| s.to_string()).collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_clean_json() {
        let raw = r#"["What database?", "What auth method?", "What deployment target?"]"#;
        let questions = parse_questions(raw);
        assert_eq!(questions.len(), 3);
        assert_eq!(questions[0], "What database?");
        assert_eq!(questions[2], "What deployment target?");
    }

    #[test]
    fn test_parse_json_with_markdown_fences() {
        let raw = "```json\n[\"Question 1?\", \"Question 2?\"]\n```";
        let questions = parse_questions(raw);
        assert_eq!(questions.len(), 2);
        assert_eq!(questions[0], "Question 1?");
    }

    #[test]
    fn test_parse_plain_code_fence() {
        let raw = "```\n[\"Q1?\", \"Q2?\", \"Q3?\"]\n```";
        let questions = parse_questions(raw);
        assert_eq!(questions.len(), 3);
    }

    #[test]
    fn test_parse_invalid_json_returns_fallback() {
        let raw = "here are some questions: ...";
        let questions = parse_questions(raw);
        // Should return fallback questions
        assert!(!questions.is_empty());
        assert!(questions[0].contains("primary goal") || questions[0].contains("goal"));
    }

    #[test]
    fn test_parse_empty_array_returns_fallback() {
        let questions = parse_questions("[]");
        assert!(!questions.is_empty());
    }

    #[test]
    fn test_parse_extra_whitespace() {
        let raw = r#"  ["Q1?", "Q2?"]  "#;
        let questions = parse_questions(raw);
        assert_eq!(questions.len(), 2);
    }

    #[test]
    fn test_no_truncation_of_long_list() {
        // If LLM generates 15 questions, all 15 should be returned
        let items: Vec<String> = (1..=15).map(|i| format!("Question {}?", i)).collect();
        let raw = serde_json::to_string(&items).unwrap();
        let questions = parse_questions(&raw);
        assert_eq!(questions.len(), 15);
    }

    #[test]
    fn test_fallback_questions_are_valid() {
        for q in FALLBACK_QUESTIONS {
            assert!(!q.is_empty());
            assert!(q.ends_with('?'));
        }
    }
}
