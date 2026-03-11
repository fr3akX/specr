# specr — Phase 3: agent-runner

Phases 1 and 2 are complete (109 tests passing). Now build Phase 3.

---

## Context

Read ALL existing source files before writing anything. The codebase at `/Users/maris/Projects/specr` has:
- `src/types.rs` — Task, TaskStatus, TaskSize, Finding, Verdict, etc.
- `src/config.rs` — config loading
- `src/store.rs` — read/write SPEC.md, TASKS.md, task detail files
- `src/llm/` — LLM trait + Anthropic/OpenAI clients
- `src/spec_composer/` — Phase 1: spec-composer
- `src/task_generator/` — Phase 2: task-generator, dependency graph, renderer
- `src/tui/` — Phase 2: ratatui task board
- `src/main.rs` — CLI with clap

Extend, don't rewrite.

---

## Phase 3 scope — agent-runner

Implement:
1. `specr run` — runs next eligible task(s) through the full review pipeline
2. `specr run --task NNN` — runs a specific task
3. Background Telegram bot daemon (when configured)

---

## New files to add

```
src/
└── agent_runner/
    ├── mod.rs              # task execution orchestration
    ├── resolver.rs         # dependency resolver (next eligible tasks)
    ├── coding_agent.rs     # subprocess: spawn Claude Code, stream output
    ├── review.rs           # parallel review agents (3x async LLM calls)
    └── loop_controller.rs  # iteration counter, pass/fail logic
src/telegram/
    ├── mod.rs              # bot setup + message routing
    └── notify.rs           # send progress/completion notifications
```

Update `src/main.rs` to add `run` subcommand and `bot` subcommand.
Update `src/store.rs` if needed for task state updates.
Update `src/config.rs` if needed for telegram config.

---

## agent_runner/resolver.rs

```rust
pub struct Resolver;

impl Resolver {
    /// Returns all tasks eligible to run:
    /// - status = Open
    /// - size = S or M (L tasks blocked)
    /// - all dependencies have status = Done
    pub fn eligible(tasks: &[Task]) -> Vec<&Task>

    /// Returns groups of tasks that can run in parallel
    /// (same eligibility criteria, no shared file dependencies)
    pub fn parallel_groups(tasks: &[Task]) -> Vec<Vec<&Task>>
}
```

---

## agent_runner/coding_agent.rs

Spawns Claude Code CLI as a subprocess. Streams stdout to terminal in real time.

```rust
pub struct CodingAgent {
    bin: String,  // from config.agent.runner_bin, default "claude"
}

impl CodingAgent {
    /// Spawn Claude Code with SPEC.md + task file as context.
    /// Streams output to terminal.
    /// Returns on completion.
    pub async fn run(
        &self,
        task: &Task,
        spec_path: &Path,
        workdir: &Path,
    ) -> Result<()>
}
```

The prompt passed to `claude`:
```
You are implementing task {id}: {name}

SPEC.md:
{spec_content}

Task details:
{task_detail_content}

Implement exactly what is described. Stay within the scope defined in "What NOT to change".
When done, run: cargo test && cargo clippy
```

Use `tokio::process::Command` to spawn. Stream stdout/stderr line by line to terminal.
Branch must already exist before calling run (created by orchestrator in mod.rs).

---

## agent_runner/review.rs

Three concurrent LLM API calls, each with a different system prompt and the git diff as context.

```rust
pub struct ReviewResult {
    pub code_review: Finding,
    pub qa_review:   Finding,
    pub style_review: Finding,
}

pub async fn run_reviews(
    llm: &dyn LlmClient,
    spec_content: &str,
    task_detail: &str,
    diff: &str,
) -> Result<ReviewResult>
```

All three fire with `tokio::try_join!`.

Each reviewer gets:
- System prompt (role-specific, see below)
- User prompt: SPEC.md content + task detail + git diff

### System prompts

**Code Review:**
```
You are a senior software engineer doing a code review. You receive a SPEC.md, a task definition,
and a git diff. Evaluate:
- Does the implementation match the spec contract?
- Are there correctness bugs or security issues?
- Are error cases handled?
- Are all "done when" criteria met?

Output JSON only:
{"verdict":"pass|fail","critical":["..."],"warnings":["..."],"suggestions":["..."]}
Critical = must fix before merge. Warnings = should fix. Suggestions = optional.
```

**QA Review:**
```
You are a QA engineer reviewing unit tests. You receive a SPEC.md, a task definition, and a git diff.
Evaluate:
- Do the tests actually test behaviour, not just lines?
- Are edge cases covered?
- Would any test pass if the implementation was subtly wrong?
- Is 90% coverage achievable with these tests?

Output JSON only:
{"verdict":"pass|fail","critical":["..."],"warnings":["..."],"suggestions":["..."]}
```

**Style Review:**
```
You are a code quality reviewer. You receive a SPEC.md, a task definition, and a git diff.
Evaluate:
- Is the code unnecessarily complex?
- Are there simpler ways to express the same logic?
- Are names clear and consistent?
- Are there any obvious refactoring opportunities?

Output JSON only:
{"verdict":"pass|fail","critical":["..."],"warnings":["..."],"suggestions":["..."]}
Style issues are rarely critical — only mark critical if the code is genuinely unreadable or
has a major structural problem.
```

Parse each response as JSON into `Finding { verdict, critical, warnings, suggestions }`.
If JSON parsing fails, treat as `verdict: fail` with one critical: "Review agent returned invalid JSON".

---

## agent_runner/loop_controller.rs

```rust
pub struct LoopController {
    max_iterations: u32,  // from config.spec.max_loop_iterations (default 5)
    current: u32,
}

impl LoopController {
    pub fn new(max: u32) -> Self
    pub fn increment(&mut self) -> Result<(), LoopLimitError>  // Err if > max
    pub fn iteration(&self) -> u32

    /// Returns true if all three reviews passed
    pub fn all_passed(result: &ReviewResult) -> bool

    /// Merges all findings into a single prompt for the coding agent
    pub fn findings_prompt(result: &ReviewResult) -> String
}
```

---

## agent_runner/mod.rs — orchestration

```
specr run [--task NNN]
  1. Read TASKS.md
  2. Resolver picks eligible tasks (or uses --task if specified)
  3. If no eligible tasks → print reason and exit
  4. For each eligible task (parallel if parallel_groups allows):
     a. Mark task as in-progress in TASKS.md
     b. Create git branch: task/NNN-name (git checkout -b)
     c. Send Telegram notification: "Starting task NNN: {name}"
     d. Spawn coding agent (streams to terminal)
     e. Get git diff (git diff main..HEAD)
     f. Run 3 parallel reviews
     g. LoopController checks results:
        - All pass → merge branch, mark done, notify Telegram
        - Any critical → send findings prompt to coding agent (re-run from d)
        - iteration > max → mark failed, preserve branch, notify Telegram with full history
     h. Print summary
```

Git operations use `tokio::process::Command` calling `git` CLI:
- `git checkout -b task/NNN-name`
- `git diff main..HEAD`
- `git checkout main && git merge task/NNN-name && git branch -d task/NNN-name`
- On failure: `git checkout main` (leave branch intact)

---

## telegram/mod.rs + telegram/notify.rs

Optional — only active when `telegram.enabled = true` in config.

```rust
pub struct TelegramNotifier {
    bot_token: String,
    chat_id: String,
}

impl TelegramNotifier {
    pub async fn send(&self, message: &str) -> Result<()>
}
```

Use `reqwest` to call the Telegram Bot API `sendMessage` endpoint directly.
No `teloxide` dependency needed for simple notifications.

`specr bot` subcommand: starts a polling loop that listens for commands:
- `status` → reads TASKS.md and replies with a text task board
- `run` → triggers `specr run` and streams progress updates
- `run NNN` → runs specific task

---

## config.rs additions

Ensure `telegram` section is handled:
```toml
[telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"
chat_id_env = "TELEGRAM_CHAT_ID"
enabled = false
autostart = false
```

Add `agent` section if not already present:
```toml
[agent]
runner = "claude-code"
runner_bin = "claude"
stream_output = true
```

---

## main.rs additions

```rust
Commands::Run { task } => agent_runner::run(&config, task.as_deref()).await,
Commands::Bot => telegram::run_bot(&config).await,
```

---

## Code quality requirements

- 90% unit test coverage
- Tests for: resolver (eligible tasks, L blocked, dep enforcement, parallel groups),
  loop_controller (pass/fail logic, limit enforcement, findings_prompt format),
  review JSON parsing (valid + invalid JSON handling),
  git command construction
- No unwrap() in production paths
- cargo clippy clean
- Note: coding_agent subprocess and actual LLM review calls don't need integration tests,
  but mock-based unit tests are expected for the orchestration logic

---

## Done when

- `specr run` picks next eligible task(s), spawns Claude Code, runs 3 parallel reviews, loops on critical findings, marks done/failed, notifies Telegram
- `specr run --task 002` runs a specific task by ID
- `specr bot` starts Telegram bot polling loop (when telegram.enabled = true)
- Resolver correctly blocks L tasks and tasks with unmet dependencies
- Loop limit (5 iterations default) enforced; failed tasks preserved on branch
- `cargo test` passes with ≥90% coverage
- `cargo clippy` clean

---

When completely finished, run:
openclaw system event --text "Done: specr Phase 3 agent-runner built" --mode now
