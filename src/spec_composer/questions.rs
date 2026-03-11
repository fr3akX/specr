/// The fixed ordered set of clarifying questions for spec composition.
/// Each question maps to a section of the SPEC.md template.
pub const QUESTIONS: &[&str] = &[
    "What does this project do? (goal)",
    "What's explicitly out of scope?",
    "What language/runtime and framework?",
    "What are the main data entities and their relationships?",
    "What are the key API endpoints or function signatures?",
    "What are the 2–3 most important user workflows?",
    "Any performance, security, or integration constraints?",
    "Any open questions or decisions not yet made?",
];

/// Returns questions up to the given budget.
pub fn get_questions(budget: usize) -> &'static [&'static str] {
    let count = budget.min(QUESTIONS.len());
    &QUESTIONS[..count]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_questions_count() {
        assert_eq!(QUESTIONS.len(), 8);
    }

    #[test]
    fn test_questions_order() {
        assert!(QUESTIONS[0].contains("goal"));
        assert!(QUESTIONS[1].contains("scope"));
        assert!(QUESTIONS[2].contains("language"));
        assert!(QUESTIONS[3].contains("data entities"));
        assert!(QUESTIONS[4].contains("API endpoints"));
        assert!(QUESTIONS[5].contains("workflows"));
        assert!(QUESTIONS[6].contains("constraints"));
        assert!(QUESTIONS[7].contains("open questions"));
    }

    #[test]
    fn test_get_questions_full_budget() {
        let q = get_questions(8);
        assert_eq!(q.len(), 8);
    }

    #[test]
    fn test_get_questions_limited_budget() {
        let q = get_questions(3);
        assert_eq!(q.len(), 3);
        assert!(q[0].contains("goal"));
        assert!(q[2].contains("language"));
    }

    #[test]
    fn test_get_questions_exceeds_total() {
        let q = get_questions(100);
        assert_eq!(q.len(), 8);
    }

    #[test]
    fn test_get_questions_zero() {
        let q = get_questions(0);
        assert_eq!(q.len(), 0);
    }

    #[test]
    fn test_questions_not_empty() {
        for q in QUESTIONS {
            assert!(!q.is_empty());
        }
    }
}
