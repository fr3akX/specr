# specr — Phase 1: spec-composer

Build Phase 1 of `specr`, a standalone Rust CLI tool that turns a rough idea into a
structured, machine-readable `SPEC.md` through a guided Q&A session.

---

## Binary name
`specr`

## Phase 1 scope — spec-composer only

Implement:
1. `specr compose "<idea>"` — guided Q&A → drafts SPEC.md → explicit approval → writes file
2. `specr refine` — re-opens existing SPEC.md for section editing, bumps spec-version on approval

Do NOT implement task-generator, agent-runner, ratatui TUI, or Telegram bot in this phase.

---

## Project structure

```
specr/
├── Cargo.toml
├── src/
│   ├── main.rs           # CLI entry + command dispatch (clap)
│   ├── config.rs         # config loading: ~/.config/specr/config.toml + env vars
│   ├── types.rs          # shared types
│   ├── store.rs          # read/write SPEC.md, TASKS.md, task files
│   ├── llm/
│   │   ├── mod.rs        # LLM trait: async fn complete(system, user) -> Result<String>
│   │   ├── anthropic.rs  # Anthropic Messages API client
│   │   └── openai.rs     # OpenAI Chat Completions client
│   └── spec_composer/
│       ├── mod.rs        # Q&A pipeline orchestration
│       ├── questions.rs  # question set + priority order
│       └── renderer.rs   # SPEC.md template rendering
```

---

## Dependencies (Cargo.toml)

```toml
[dependencies]
tokio = { version = "1", features = ["full"] }
clap = { version = "4", features = ["derive"] }
serde = { version = "1", features = ["derive"] }
serde_json = "1"
toml = "0.8"
reqwest = { version = "0.12", features = ["json"] }
anyhow = "1"
dirs = "5"
chrono = { version = "0.4", features = ["serde"] }
colored = "2"
```

---

## config.rs

Load from `~/.config/specr/config.toml`. Create with defaults if missing.

```toml
[llm]
provider = "anthropic"           # anthropic | openai
model = "claude-sonnet-4-6"
api_key_env = "ANTHROPIC_API_KEY"

[output]
base_dir = "."
obsidian_dir = ""                # optional, empty = disabled

[spec]
question_budget = 8
```

All secrets via env vars — config file never stores values, only env var names.

---

## LLM trait (llm/mod.rs)

```rust
#[async_trait]
pub trait LlmClient: Send + Sync {
    async fn complete(&self, system: &str, user: &str) -> Result<String>;
}
```

Implement for Anthropic (Messages API) and OpenAI (Chat Completions).
Provider selected from config. API key read from the env var named in config.

---

## spec_composer: Q&A pipeline

### questions.rs

Fixed ordered question set, asked one at a time (max 8 = question_budget from config):

1. What does this project do? (goal)
2. What's explicitly out of scope?
3. What language/runtime and framework?
4. What are the main data entities and their relationships?
5. What are the key API endpoints or function signatures?
6. What are the 2–3 most important user workflows?
7. Any performance, security, or integration constraints?
8. Any open questions or decisions not yet made?

### mod.rs — pipeline

```
compose("<idea>")
  1. Print welcome + initial idea back to user
  2. For each question (up to budget):
       - Print question with number indicator e.g. "[2/8]"
       - Read answer from stdin
       - If answer is empty/skip → note as unanswered, continue
  3. Call LLM with system prompt + all Q&A pairs → draft SPEC.md content
  4. Print full draft to terminal with clear header "--- DRAFT SPEC.md ---"
  5. Ask: "Approve? (yes / edit <section> / no)"
       - "yes" / "approve" / "y" → write file, done
       - "edit <section>" → prompt for new content for that section, regenerate, re-show
       - "no" → abort, nothing written
  6. On approval: write SPEC.md, print path
```

### LLM prompt for drafting

System prompt:
```
You are a senior software architect. Given a project idea and answers to clarifying questions,
produce a complete, concise SPEC.md in the exact format provided. Be specific and concrete.
Fill in any unanswered sections with a reasonable assumption and mark it "(assumed)".
Output ONLY the markdown content, no commentary.
```

User prompt: initial idea + all Q&A pairs + the SPEC.md template.

---

## SPEC.md template (renderer.rs)

```markdown
---
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
```

---

## store.rs

- `read_spec(dir: &Path) -> Result<String>` — reads SPEC.md from dir
- `write_spec(dir: &Path, content: &str) -> Result<()>` — writes SPEC.md
- If `obsidian_dir` is set in config, also writes there under `01_Projects/{project_name}/SPEC.md`
- `bump_spec_version(content: &str) -> String` — increments spec-version in frontmatter

---

## specr refine

1. Read existing SPEC.md from cwd
2. Show current content
3. Ask which section to edit
4. Replace that section's content (user types new content)
5. Call LLM to ensure consistency across sections if needed (optional, best effort)
6. Show updated draft
7. Approval gate → write on approve, bump spec-version

---

## CLI (main.rs with clap)

```
specr compose "<idea>"   # start Q&A
specr refine             # refine existing spec
specr --help
```

---

## Error handling

- Missing API key → clear error: "Set ANTHROPIC_API_KEY (or whichever provider) in your environment"
- Missing SPEC.md for refine → "No SPEC.md found in current directory. Run: specr compose \"<idea>\""
- LLM API error → print error + suggestion to retry
- Use anyhow for error propagation throughout

---

## Code quality requirements

- 90% unit test coverage
- Tests for: config loading, question ordering, SPEC.md rendering, spec-version bumping, store read/write
- No unwrap() in production paths — use ? and anyhow
- Clear comments on non-obvious logic
- No hardcoded secrets anywhere

---

## Done when

- `specr compose "build a REST API for managing todos"` runs end-to-end, asks 8 questions, drafts SPEC.md, writes on approval
- `specr refine` loads existing SPEC.md, edits a section, bumps spec-version on approval
- Both Anthropic and OpenAI backends compile and have unit tests
- `cargo test` passes with ≥90% coverage
- `cargo clippy` clean

---

When completely finished, run:
openclaw system event --text "Done: specr Phase 1 spec-composer built" --mode now
