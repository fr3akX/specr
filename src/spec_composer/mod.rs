pub mod questions;
pub mod renderer;

#[cfg(test)]
use std::io::BufRead;
use std::io::{self, Write};
use std::path::Path;

use anyhow::Result;
use colored::Colorize;
use rustyline::DefaultEditor;

use crate::config::Config;
use crate::llm::LlmClient;
use crate::store;

// ---------------------------------------------------------------------------
// LineInput trait — abstracts readline for production (rustyline) vs tests
// ---------------------------------------------------------------------------

/// Abstraction over line input. Returns the trimmed line, or empty string on
/// skip / EOF. Never returns an error for normal empty input.
pub trait LineInput {
    fn readline(&mut self, prompt: &str) -> Result<String>;
}

/// Production implementation backed by rustyline (arrow keys, history, etc.)
pub struct RustylineInput {
    editor: DefaultEditor,
}

impl RustylineInput {
    pub fn new() -> Result<Self> {
        let editor =
            DefaultEditor::new().map_err(|e| anyhow::anyhow!("Failed to init readline: {e}"))?;
        Ok(Self { editor })
    }
}

impl LineInput for RustylineInput {
    fn readline(&mut self, prompt: &str) -> Result<String> {
        match self.editor.readline(prompt) {
            Ok(line) => {
                // Add non-empty lines to history for ↑ recall
                if !line.trim().is_empty() {
                    let _ = self.editor.add_history_entry(&line);
                }
                Ok(line.trim().to_string())
            }
            // EOF / Ctrl-D treated as empty (skip)
            Err(rustyline::error::ReadlineError::Eof)
            | Err(rustyline::error::ReadlineError::Interrupted) => Ok(String::new()),
            Err(e) => Err(anyhow::anyhow!("Readline error: {e}")),
        }
    }
}

/// Test implementation backed by any `BufRead`.
#[cfg(test)]
pub struct BufReadInput<R: BufRead> {
    reader: R,
}

#[cfg(test)]
impl<R: BufRead> BufReadInput<R> {
    pub fn new(reader: R) -> Self {
        Self { reader }
    }
}

#[cfg(test)]
impl<R: BufRead> LineInput for BufReadInput<R> {
    fn readline(&mut self, _prompt: &str) -> Result<String> {
        let mut line = String::new();
        self.reader.read_line(&mut line)?;
        Ok(line.trim().to_string())
    }
}

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
    let mut input = RustylineInput::new()?;
    compose_with_input(
        idea,
        config,
        client,
        output_dir,
        &mut input,
        &mut io::stdout(),
    )
    .await
}

/// Compose with injectable I/O for testing.
#[cfg(test)]
pub async fn compose_with_io<R: BufRead, W: Write>(
    idea: &str,
    config: &Config,
    client: &dyn LlmClient,
    output_dir: &Path,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    let mut input = BufReadInput::new(reader);
    compose_with_input(idea, config, client, output_dir, &mut input, writer).await
}

/// Internal implementation shared by compose() and compose_with_io().
async fn compose_with_input<L: LineInput, W: Write>(
    idea: &str,
    config: &Config,
    client: &dyn LlmClient,
    output_dir: &Path,
    input: &mut L,
    writer: &mut W,
) -> Result<()> {
    // Step 1: Welcome
    writeln!(writer, "\n{}", "=== specr compose ===".bold().cyan())?;
    writeln!(writer, "Idea: {}\n", idea.bold())?;
    writeln!(
        writer,
        "I'll ask tailored clarifying questions. Press Enter to skip any.\n"
    )?;

    // Step 2: Generate questions from LLM
    writeln!(writer, "{}", "Generating questions...".dimmed())?;
    writer.flush()?;
    let question_list = questions::generate(idea, config.spec.question_budget, client).await;
    let mut qa_pairs: Vec<(String, Option<String>)> = Vec::new();

    for (i, question) in question_list.iter().enumerate() {
        writeln!(
            writer,
            "{} {}",
            format!("[{}/{}]", i + 1, question_list.len()).dimmed(),
            question.yellow()
        )?;
        let answer = input.readline("> ")?;

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
        writeln!(writer, "{}", "Approve? (yes / edit <section> / no)".bold())?;

        let approval_raw = input.readline("> ")?.to_lowercase();
        let approval = approval_raw.as_str();

        if approval == "yes" || approval == "y" || approval == "approve" || approval == "ok" {
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
        } else if approval == "no" || approval == "n" {
            writeln!(writer, "{}", "Aborted. No file written.".yellow())?;
            return Ok(());
        } else if let Some(section) = approval.strip_prefix("edit ") {
            let section = section.trim();
            // Validate section name
            let matched = renderer::SECTION_HEADINGS
                .iter()
                .find(|h| h.eq_ignore_ascii_case(section));

            if let Some(heading) = matched {
                writeln!(
                    writer,
                    "\nEnter new content for section '{}' (empty line to finish):",
                    heading.bold()
                )?;
                let mut new_content = String::new();
                loop {
                    let line = input.readline("")?;
                    if line.is_empty() {
                        break;
                    }
                    new_content.push_str(&line);
                    new_content.push('\n');
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
pub async fn refine(config: &Config, client: &dyn LlmClient, dir: &Path) -> Result<()> {
    let mut input = RustylineInput::new()?;
    refine_with_input(config, client, dir, &mut input, &mut io::stdout()).await
}

/// Refine with injectable I/O for testing.
#[cfg(test)]
pub async fn refine_with_io<R: BufRead, W: Write>(
    config: &Config,
    client: &dyn LlmClient,
    dir: &Path,
    reader: &mut R,
    writer: &mut W,
) -> Result<()> {
    let mut input = BufReadInput::new(reader);
    refine_with_input(config, client, dir, &mut input, writer).await
}

/// Internal implementation shared by refine() and refine_with_io().
async fn refine_with_input<L: LineInput, W: Write>(
    config: &Config,
    client: &dyn LlmClient,
    dir: &Path,
    input: &mut L,
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
        let line = input.readline("> ")?;
        let line = line.as_str();

        if line.eq_ignore_ascii_case("done")
            || line.eq_ignore_ascii_case("yes")
            || line.eq_ignore_ascii_case("approve")
            || line.eq_ignore_ascii_case("ok")
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
        } else if line.eq_ignore_ascii_case("abort")
            || line.eq_ignore_ascii_case("no")
            || line.eq_ignore_ascii_case("cancel")
        {
            writeln!(writer, "{}", "Aborted. No changes saved.".yellow())?;
            return Ok(());
        }

        // Step 4: Validate section
        let matched = renderer::SECTION_HEADINGS
            .iter()
            .find(|h| h.eq_ignore_ascii_case(line));

        if let Some(heading) = matched {
            writeln!(
                writer,
                "\nEnter new content for section '{}' (empty line to finish):",
                heading.bold()
            )?;
            let mut new_content = String::new();
            loop {
                let section_line = input.readline("")?;
                if section_line.is_empty() {
                    break;
                }
                new_content.push_str(&section_line);
                new_content.push('\n');
            }

            // Replace the section
            content = renderer::replace_section(&content, heading, &new_content);

            // Step 5: Best-effort LLM consistency check
            let consistency_result = check_consistency(client, &content).await;
            if let Ok(Some(suggestion)) = consistency_result {
                writeln!(writer, "\n{}", "LLM consistency note:".cyan())?;
                writeln!(writer, "{}\n", suggestion)?;
            }
        } else if !line.is_empty() {
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

    /// A mock LLM client.
    /// First call returns questions JSON, subsequent calls return the spec draft.
    struct MockLlmClient {
        questions_response: String,
        spec_response: String,
        call_count: std::sync::atomic::AtomicUsize,
    }

    impl MockLlmClient {
        /// Simple mock: always returns the same response for any call.
        fn new(response: &str) -> Self {
            Self {
                // First call = generate_extras: return 1 extra question on top of seeds
                questions_response: r#"["What is the expected data volume?"]"#.to_string(),
                spec_response: response.to_string(),
                call_count: std::sync::atomic::AtomicUsize::new(0),
            }
        }
    }

    #[async_trait]
    impl LlmClient for MockLlmClient {
        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            let n = self
                .call_count
                .fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            if n == 0 {
                // First call: question generation
                Ok(self.questions_response.clone())
            } else {
                // Subsequent calls: spec draft or consistency check
                Ok(self.spec_response.clone())
            }
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

        // Mock: 5 seeds + 1 extra = 6 questions; answer q1, skip rest, then approve
        let input = "A todo app\n\n\n\n\n\ny\n";
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        compose_with_io(
            "Build a todo app",
            &config,
            &client,
            tmp.path(),
            &mut reader,
            &mut output,
        )
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

        // Mock: 6 questions; provide 6 answers then abort
        let input = "answer1\nanswer2\nanswer3\nanswer4\nanswer5\nanswer6\nno\n";
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        compose_with_io(
            "idea",
            &config,
            &client,
            tmp.path(),
            &mut reader,
            &mut output,
        )
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

        // Mock: 6 questions; skip all, then approve
        let input = "\n\n\n\n\n\ny\n";
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut output = Vec::new();

        compose_with_io(
            "idea",
            &config,
            &client,
            tmp.path(),
            &mut reader,
            &mut output,
        )
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

    /// A simple mock that always returns the same response regardless of call count.
    struct AlwaysMock(String);

    #[async_trait]
    impl LlmClient for AlwaysMock {
        async fn complete(&self, _system: &str, _user: &str) -> Result<String> {
            Ok(self.0.clone())
        }
    }

    #[tokio::test]
    async fn test_check_consistency_ok() {
        let client = AlwaysMock("OK".to_string());
        let result = check_consistency(&client, "spec content").await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn test_check_consistency_with_issues() {
        let client =
            AlwaysMock("The stack section mentions Python but the goal says Rust.".to_string());
        let result = check_consistency(&client, "spec content").await.unwrap();
        assert!(result.is_some());
        assert!(result.unwrap().contains("Python"));
    }
}
