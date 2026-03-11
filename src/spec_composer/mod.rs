pub mod questions;
pub mod renderer;

use std::io::{self, BufRead, Write};
use std::path::Path;

use anyhow::Result;
use colored::Colorize;

use crate::config::Config;
use crate::llm::LlmClient;
use crate::store;

const SYSTEM_PROMPT: &str = "\
You are a senior software architect. Given a project idea and answers to clarifying questions, \
produce a complete, concise SPEC.md in the exact format provided. Be specific and concrete. \
Fill in any unanswered sections with a reasonable assumption and mark it \"(assumed)\". \
Output ONLY the markdown content, no commentary.";

/// Run the compose pipeline: Q&A -> LLM draft -> approval -> write file.
pub async fn compose(
    idea: &str,
    config: &Config,
    client: &dyn LlmClient,
    output_dir: &Path,
) -> Result<()> {
    compose_with_io(idea, config, client, output_dir, &mut io::stdin().lock(), &mut io::stdout())
        .await
}

/// Compose with injectable I/O for testing.
pub async fn compose_with_io<R: BufRead, W: Write>(
    idea: &str,
    config: &Config,
    client: &dyn LlmClient,
    output_dir: &Path,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    // Step 1: Welcome
    writeln!(writer, "\n{}", "=== specr compose ===".bold().cyan())?;
    writeln!(writer, "Idea: {}\n", idea.bold())?;
    writeln!(
        writer,
        "I'll ask up to {} clarifying questions. Press Enter to skip any.\n",
        config.spec.question_budget
    )?;

    // Step 2: Q&A
    let question_list = questions::get_questions(config.spec.question_budget);
    let mut qa_pairs: Vec<(String, Option<String>)> = Vec::new();

    for (i, question) in question_list.iter().enumerate() {
        writeln!(
            writer,
            "{} {}",
            format!("[{}/{}]", i + 1, question_list.len()).dimmed(),
            question.yellow()
        )?;
        write!(writer, "> ")?;
        writer.flush()?;

        let mut answer = String::new();
        reader.read_line(&mut answer)?;
        let answer = answer.trim().to_string();

        if answer.is_empty() || answer.eq_ignore_ascii_case("skip") {
            writeln!(writer, "{}", "(skipped)".dimmed())?;
            qa_pairs.push((question.to_string(), None));
        } else {
            qa_pairs.push((question.to_string(), Some(answer)));
        }
        writeln!(writer)?;
    }

    // Step 3: Call LLM
    writeln!(writer, "{}", "Generating SPEC.md draft...".cyan())?;
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let user_prompt = renderer::build_user_prompt(idea, &qa_pairs, &today);
    let mut draft = client.complete(SYSTEM_PROMPT, &user_prompt).await?;

    // Approval loop
    loop {
        // Step 4: Show draft
        writeln!(writer, "\n{}\n", "--- DRAFT SPEC.md ---".bold().green())?;
        writeln!(writer, "{}", draft)?;
        writeln!(writer, "{}\n", "--- END DRAFT ---".bold().green())?;

        // Step 5: Ask for approval
        writeln!(
            writer,
            "{}",
            "Approve? (yes / edit <section> / no)".bold()
        )?;
        write!(writer, "> ")?;
        writer.flush()?;

        let mut input = String::new();
        reader.read_line(&mut input)?;
        let input = input.trim().to_lowercase();

        if input == "yes" || input == "y" || input == "approve" {
            // Step 6: Write file
            store::write_spec(output_dir, &draft, config)?;
            let spec_path = output_dir.join("SPEC.md");
            writeln!(
                writer,
                "\n{} {}",
                "SPEC.md written to".green(),
                spec_path.display().to_string().bold()
            )?;
            return Ok(());
        } else if input == "no" || input == "n" {
            writeln!(writer, "{}", "Aborted. No file written.".yellow())?;
            return Ok(());
        } else if let Some(section) = input.strip_prefix("edit ") {
            let section = section.trim();
            // Validate section name
            let matched = renderer::SECTION_HEADINGS
                .iter()
                .find(|h| h.eq_ignore_ascii_case(section));

            if let Some(heading) = matched {
                writeln!(
                    writer,
                    "\nEnter new content for section '{}' (end with an empty line):",
                    heading.bold()
                )?;
                let mut new_content = String::new();
                loop {
                    let mut line = String::new();
                    reader.read_line(&mut line)?;
                    if line.trim().is_empty() {
                        break;
                    }
                    new_content.push_str(&line);
                }
                draft = renderer::replace_section(&draft, heading, &new_content);
            } else {
                writeln!(writer, "{}", "Unknown section. Available sections:".red())?;
                for heading in renderer::SECTION_HEADINGS {
                    writeln!(writer, "  - {}", heading)?;
                }
            }
        } else {
            writeln!(
                writer,
                "{}",
                "Please enter 'yes', 'edit <section>', or 'no'.".yellow()
            )?;
        }
    }
}

/// Run the refine pipeline: load existing SPEC.md, edit sections, re-approve.
pub async fn refine(
    config: &Config,
    client: &dyn LlmClient,
    dir: &Path,
) -> Result<()> {
    refine_with_io(config, client, dir, &mut io::stdin().lock(), &mut io::stdout())
        .await
}

/// Refine with injectable I/O for testing.
pub async fn refine_with_io<R: BufRead, W: Write>(
    config: &Config,
    client: &dyn LlmClient,
    dir: &Path,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    // Step 1: Read existing SPEC.md
    let mut content = store::read_spec(dir)?;

    writeln!(writer, "\n{}", "=== specr refine ===".bold().cyan())?;

    // Refinement loop
    loop {
        // Step 2: Show current content
        writeln!(writer, "\n{}\n", "--- Current SPEC.md ---".bold().green())?;
        writeln!(writer, "{}", content)?;
        writeln!(writer, "{}\n", "--- END ---".bold().green())?;

        // Step 3: Ask which section to edit
        writeln!(writer, "Available sections:")?;
        for heading in renderer::SECTION_HEADINGS {
            writeln!(writer, "  - {}", heading)?;
        }
        writeln!(
            writer,
            "\n{}",
            "Enter section to edit (or 'done' to approve / 'abort' to cancel):".bold()
        )?;
        write!(writer, "> ")?;
        writer.flush()?;

        let mut input = String::new();
        reader.read_line(&mut input)?;
        let input = input.trim();

        if input.eq_ignore_ascii_case("done")
            || input.eq_ignore_ascii_case("yes")
            || input.eq_ignore_ascii_case("approve")
        {
            // Step 7: Bump version and write
            content = store::bump_spec_version(&content);
            store::write_spec(dir, &content, config)?;
            let spec_path = dir.join("SPEC.md");
            writeln!(
                writer,
                "\n{} {}",
                "SPEC.md updated at".green(),
                spec_path.display().to_string().bold()
            )?;
            return Ok(());
        } else if input.eq_ignore_ascii_case("abort")
            || input.eq_ignore_ascii_case("no")
            || input.eq_ignore_ascii_case("cancel")
        {
            writeln!(writer, "{}", "Aborted. No changes saved.".yellow())?;
            return Ok(());
        }

        // Step 4: Validate section
        let matched = renderer::SECTION_HEADINGS
            .iter()
            .find(|h| h.eq_ignore_ascii_case(input));

        if let Some(heading) = matched {
            writeln!(
                writer,
                "\nEnter new content for section '{}' (end with an empty line):",
                heading.bold()
            )?;
            let mut new_content = String::new();
            loop {
                let mut line = String::new();
                reader.read_line(&mut line)?;
                if line.trim().is_empty() {
                    break;
                }
                new_content.push_str(&line);
            }

            // Replace the section
            content = renderer::replace_section(&content, heading, &new_content);

            // Step 5: Best-effort LLM consistency check
            let consistency_result = check_consistency(client, &content).await;
            if let Ok(Some(suggestion)) = consistency_result {
                writeln!(writer, "\n{}", "LLM consistency note:".cyan())?;
                writeln!(writer, "{}\n", suggestion)?;
            }
        } else {
            writeln!(
                writer,
                "{}",
                "Unknown section. Please enter one of the listed sections.".red()
            )?;
        }
    }
}

/// Best-effort consistency check via LLM after editing a section.
async fn check_consistency(client: &dyn LlmClient, spec: &str) -> Result<Option<String>> {
    let system = "You are reviewing a SPEC.md for internal consistency. \
        If all sections are consistent with each other, respond with exactly 'OK'. \
        If there are inconsistencies, briefly note them (1-2 sentences max).";
    let response = client.complete(system, spec).await;

    match response {
        Ok(text) if text.trim() == "OK" => Ok(None),
        Ok(text) => Ok(Some(text)),
        // Silently ignore errors — this is best-effort
        Err(_) => Ok(None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_trait::async_trait;

    /// A mock LLM client that returns a fixed response.
    struct MockLlmClient {
        response: String,
    }

    impl MockLlmClient {
        fn new(response: &str) -> Self {
            Self {
                response: response.to_string(),
            }
        }
    }

    #[async_trait]
    impl LlmClient for MockLlmClient {
        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(self.response.clone())
        }
    }

    fn test_config() -> Config {
        let mut config = Config::default();
        config.llm.provider = "anthropic".to_string();
        config.llm.model = "test".to_string();
        config.llm.api_key_env = "TEST".to_string();
        config.spec.question_budget = 2; // Small budget for tests
        config
    }

    #[tokio::test]
    async fn test_compose_approve() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = test_config();
        let mock_spec = "---\nspec-version: 1\n---\n\n# Project: Test\n\n## Goal\nTest goal\n";
        let client = MockLlmClient::new(mock_spec);

        // Simulate: answer q1, skip q2, then approve
        let input = "A todo app\n\ny\n";
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        compose_with_io("Build a todo app", &config, &client, tmp.path(), &mut reader, &mut output)
            .await
            .unwrap();

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains("SPEC.md written to"));
        assert!(tmp.path().join("SPEC.md").exists());
    }

    #[tokio::test]
    async fn test_compose_abort() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = test_config();
        let client = MockLlmClient::new("draft content");

        let input = "answer1\nanswer2\nno\n";
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        compose_with_io("idea", &config, &client, tmp.path(), &mut reader, &mut output)
            .await
            .unwrap();

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains("Aborted"));
        assert!(!tmp.path().join("SPEC.md").exists());
    }

    #[tokio::test]
    async fn test_compose_skip_questions() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = test_config();
        let client = MockLlmClient::new("draft");

        // Skip both questions, then approve
        let input = "\n\ny\n";
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        compose_with_io("idea", &config, &client, tmp.path(), &mut reader, &mut output)
            .await
            .unwrap();

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains("(skipped)"));
    }

    #[tokio::test]
    async fn test_refine_approve() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = test_config();
        let client = MockLlmClient::new("OK");

        // Write an initial SPEC.md
        let initial = "---\nspec-version: 1\ncreated: 2024-01-01\nupdated: 2024-01-01\n---\n\n# Project: Test\n\n## Goal\nOld goal\n\n## Scope\nOld scope\n";
        std::fs::write(tmp.path().join("SPEC.md"), initial).unwrap();

        // Edit goal section, then approve
        let input = "Goal\nNew goal content\n\ndone\n";
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        refine_with_io(&config, &client, tmp.path(), &mut reader, &mut output)
            .await
            .unwrap();

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains("SPEC.md updated"));

        let content = std::fs::read_to_string(tmp.path().join("SPEC.md")).unwrap();
        assert!(content.contains("spec-version: 2"));
        assert!(content.contains("New goal content"));
    }

    #[tokio::test]
    async fn test_refine_abort() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = test_config();
        let client = MockLlmClient::new("OK");

        let initial = "---\nspec-version: 1\n---\n\n# Project: Test\n";
        std::fs::write(tmp.path().join("SPEC.md"), initial).unwrap();

        let input = "abort\n";
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        refine_with_io(&config, &client, tmp.path(), &mut reader, &mut output)
            .await
            .unwrap();

        let output_str = String::from_utf8(output).unwrap();
        assert!(output_str.contains("Aborted"));

        // Content should be unchanged
        let content = std::fs::read_to_string(tmp.path().join("SPEC.md")).unwrap();
        assert!(content.contains("spec-version: 1"));
    }

    #[tokio::test]
    async fn test_refine_missing_spec() {
        let tmp = tempfile::TempDir::new().unwrap();
        let config = test_config();
        let client = MockLlmClient::new("OK");

        let mut reader = io::Cursor::new("".as_bytes());
        let mut output = Vec::new();

        let result = refine_with_io(&config, &client, tmp.path(), &mut reader, &mut output).await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No SPEC.md found"));
    }

    #[tokio::test]
    async fn test_check_consistency_ok() {
        let client = MockLlmClient::new("OK");
        let result = check_consistency(&client, "spec content").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_check_consistency_with_issues() {
        let client = MockLlmClient::new("The stack section mentions Python but the goal says Rust.");
        let result = check_consistency(&client, "spec content").await.unwrap();
        assert!(result.is_some());
        assert!(result.unwrap().contains("Python"));
    }
}
