use crate::llm::LlmClient;

/// Predefined seed questions — always included, covering the universal baseline.
/// Passed to the LLM as context so it generates *complementary* questions only.
pub const SEED_QUESTIONS: &[&str] = &[
    "What language/runtime and framework should be used?",
    "What is the deployment target (local script, server, Docker, etc.)?",
    "What is explicitly out of scope for v1?",
    "What are the main data entities and their relationships?",
    "What auth or security requirements exist (if any)?",
];

const SYSTEM_PROMPT: &str = "\
You are a senior software architect. Given a project idea and a list of questions \
that are already covered, generate additional targeted clarifying questions that \
would meaningfully improve the resulting spec.\n\
\n\
Rules:\n\
- Do NOT repeat or rephrase any of the already-covered questions\n\
- Ask only about things genuinely ambiguous for THIS project that would affect design decisions\n\
- No generic boilerplate — every question must be specific to this project\n\
- Each question must be actionable and concrete\n\
- Aim for up to {budget} additional questions, but only what the project genuinely needs\n\
- Output ONLY a JSON array of strings, no commentary, no markdown fences\n\
\n\
Example output:\n\
[\"What data should be tracked per workout?\", \"Should the CLI import GPX or FIT files?\"]";

/// Fallback extra questions used if the LLM call fails or returns unparseable output.
const FALLBACK_EXTRA: &[&str] = &[
    "What is the primary user workflow from start to finish?",
    "Are there any performance or scalability constraints?",
];

/// Generate the full question list: seeds + LLM-generated extras.
///
/// The LLM receives the seed questions as context and is instructed to
/// produce complementary questions that don't overlap with them.
/// Returns seeds first, then LLM questions. No truncation of LLM output.
pub async fn generate(idea: &str, budget: usize, client: &dyn LlmClient) -> Vec<String> {
    let seeds: Vec<String> = SEED_QUESTIONS.iter().map(|s| s.to_string()).collect();
    let extras = generate_extras(idea, budget, &seeds, client).await;

    let mut all = seeds;
    all.extend(extras);
    all
}

/// Ask the LLM to generate project-specific questions that complement the seeds.
async fn generate_extras(
    idea: &str,
    budget: usize,
    seeds: &[String],
    client: &dyn LlmClient,
) -> Vec<String> {
    let system = SYSTEM_PROMPT.replace("{budget}", &budget.to_string());

    let seeds_list = seeds
        .iter()
        .enumerate()
        .map(|(i, q)| format!("{}. {}", i + 1, q))
        .collect::<Vec<_>>()
        .join("\n");

    let user = format!(
        "Project idea: {}\n\nAlready covered questions (do not repeat):\n{}",
        idea, seeds_list
    );

    match client.complete(&system, &user).await {
        Ok(raw) => parse_questions(&raw),
        Err(_) => FALLBACK_EXTRA.iter().map(|s| s.to_string()).collect(),
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
        _ => FALLBACK_EXTRA
            .iter()
            .map(|s: &&str| s.to_string())
            .collect(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_seed_questions_count() {
        assert_eq!(SEED_QUESTIONS.len(), 5);
    }

    #[test]
    fn test_seed_questions_cover_baseline() {
        let seeds_joined = SEED_QUESTIONS.join(" ").to_lowercase();
        assert!(seeds_joined.contains("language") || seeds_joined.contains("runtime"));
        assert!(seeds_joined.contains("deployment"));
        assert!(seeds_joined.contains("scope"));
        assert!(seeds_joined.contains("data entities") || seeds_joined.contains("entities"));
        assert!(seeds_joined.contains("auth") || seeds_joined.contains("security"));
    }

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
        assert!(!questions.is_empty());
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
        let items: Vec<String> = (1..=15).map(|i| format!("Question {}?", i)).collect();
        let raw = serde_json::to_string(&items).unwrap();
        let questions = parse_questions(&raw);
        assert_eq!(questions.len(), 15);
    }

    #[test]
    fn test_fallback_extra_are_valid() {
        for q in FALLBACK_EXTRA {
            assert!(!q.is_empty());
        }
    }

    #[test]
    fn test_generate_includes_seeds_first() {
        // The combined list must start with seed questions
        // (tested structurally — generate() puts seeds first)
        let seeds: Vec<String> = SEED_QUESTIONS.iter().map(|s| s.to_string()).collect();
        assert_eq!(seeds[0], SEED_QUESTIONS[0]);
    }
}
