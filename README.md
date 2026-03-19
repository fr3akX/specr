# specr

Turn a rough idea into a structured, machine-readable project spec and task breakdown — then drive agentic development through a quality-gated execution loop.

```
specr compose "a REST API for managing cycling workouts"
```

---

## What it does

**specr** is a four-phase pipeline:

1. **spec-composer** — guided Q&A turns your idea into a reviewed `SPEC.md`
2. **task-generator** — decomposes `SPEC.md` into a sized, dependency-ordered task list
3. **agent-runner** — executes tasks via Claude Code or direct Anthropic API, runs parallel code/QA/style reviews, loops until clean
4. **coordinator** — optional parallel mode: runs multiple tasks concurrently in git worktrees, merges and tests each result, auto-resolves conflicts with LLM

The output is three layers:

```
SPEC.md              ← the contract (what & why)
TASKS.md             ← ordered task list with sizes and dependencies
tasks/NNN-name.md    ← per-task detail files (M and L tasks)
.specr/              ← per-agent instruction files (optional)
```

---

## Install

```bash
cargo install --path .
```

Requires Rust 1.75+. For the `claude-code` runner (default), also needs the `claude` CLI in your PATH.

---

## Quick start

```bash
# Compose a spec (interactive Q&A)
specr compose "a CLI tool to track cycling workouts"

# Decompose into tasks
specr tasks

# View task board
specr status

# Run next eligible task
specr run

# Run all tasks sequentially
specr run --all

# Run up to 4 tasks in parallel
specr run --all --jobs 4
```

---

## Commands

| Command | Description |
|---|---|
| `specr compose "<idea>"` | Guided Q&A → draft SPEC.md → editor review → write |
| `specr refine` | Re-open existing SPEC.md for editing, bumps spec-version |
| `specr tasks` | Decompose SPEC.md into TASKS.md + task detail files |
| `specr drift` | Generate tasks only for what changed in SPEC.md (committed or uncommitted) |
| `specr drift --base <ref>` | Diff against a specific git ref / tag |
| `specr status` | Interactive ratatui TUI task board |
| `specr split --task NNN` | Split an L-sized task into subtasks |
| `specr run` | Run next eligible task(s) |
| `specr run --task NNN` | Run a specific task by ID |
| `specr run --all` | Run all tasks until done or a failure stops the run |
| `specr run --all --jobs N` | Run N tasks in parallel (coordinator mode) |
| `specr instructions show` | Show effective agent instructions for the current project |
| `specr instructions init` | Scaffold `.specr/` with commented template files |
| `specr instructions generate` | LLM reads SPEC.md and writes project-tailored instructions |
| `specr bot` | Start Telegram bot daemon |

---

## How the spec-composer works

`specr compose` is a two-step Q&A + draft pipeline.

### Step 1 — Questions

Two rounds:

**Predefined seed questions** (always asked):
1. What language/runtime and framework?
2. What is the deployment target?
3. What is explicitly out of scope for v1?
4. What are the main data entities and relationships?
5. What auth or security requirements exist?

**LLM-generated extras** — the LLM receives the seeds as context and generates project-specific questions. A cycling tracker gets questions about GPX/FIT import; an invoice API gets questions about PDF generation. No overlap with seeds.

### Step 2 — Draft + editor review

After answering, specr generates a SPEC.md draft and asks:

```
Approve? [y]es / [e]dit in $EDITOR / [n]o
```

- **`y`** — write to disk.
- **`n`** — abort.
- **`e`** — opens in `$EDITOR`. Edit freely, add inline comments. On close, specr feeds your edits back to the LLM for a revised draft. Loop until satisfied.

---

## How the agent-runner works

```
specr run
  → dependency resolver picks eligible tasks (deps done, size S or M)
  → auto-detects default branch (main/master/etc.)
  → creates git branch: task/NNN-name
  → spawns coding agent (streamed to terminal)
  → on completion:
      ┌──────────────┬────────────┬────────────┐
      │  code review │ QA review  │   style    │  ← 3 parallel LLM calls
      └──────────────┴────────────┴────────────┘
      each returns: { verdict, critical, warnings, suggestions }
  → critical finding → findings + "already done" summary sent back → re-run
  → loop limit (default 10) → task marked FAILED, branch preserved
  → all pass → task marked DONE, branch merged to default
```

**Review agents** each get the git diff (filtered, not the full repo):
- **Code review**: correctness, spec compliance, security, error handling
- **QA review**: tests cover behaviour, edge cases, meaningful assertions
- **Style review**: clarity, naming, simplification

Only `critical` findings block task completion. `warnings` and `suggestions` are logged.

On retry, the agent receives: original task + review findings + a summary of what was already committed ("do not redo this").

---

## Parallel mode (coordinator)

When `--jobs N` (or `config.agent.parallel_jobs > 1`), specr switches to coordinator mode:

```
specr run --all --jobs 4
```

```
⚡ Parallel mode: max 4 jobs  test: cargo test

▶ [001] Scaffold          → /tmp/specr-worker-001/
▶ [003] Models            → /tmp/specr-worker-003/
▶ [005] Config loader     → /tmp/specr-worker-005/

⎇ [001] Worker done — merging branch task/001-scaffold
⧗ Running tests after merge...
✔ [001] Tests pass — task done ✔

▶ [007] API layer         → /tmp/specr-worker-007/  (deps: 001 ✔)
```

- Each task gets its own `git worktree` — fully isolated, no filesystem collisions
- Coordinator stays on the default branch; merges one branch at a time
- Tests run after each merge — the oracle for correctness
- Merge conflicts → LLM reads conflicted files + task scopes → resolves → tests re-run
- Integration test failures → LLM reads failing tests + source → patches → tests re-run
- If resolution fails → merge reverted, task marked failed

Configure:
```toml
[agent]
parallel_jobs = 4
worktree_base = "/tmp/specr"   # where worktrees are created
test_command = "cargo test"    # auto-detected; override here
resolve_conflicts = true       # auto-resolve with LLM (default)
```

---

## Agent runners

Two runners, same interface:

### `claude-code` (default)

Spawns the `claude` CLI subprocess. No API key needed — uses your Claude Code subscription.

```toml
[agent]
runner = "claude-code"
runner_bin = "claude"
```

### `api-agent`

Direct Anthropic API tool-use loop. No `claude` binary required. Requires an API key or OAuth token.

```toml
[agent]
runner = "api-agent"
max_agent_turns = 30   # max tool-call iterations per task
```

Tools available to the api-agent: `read_file`, `write_file`, `edit_file`, `run_command`.

Context compaction kicks in at 150K input tokens: keeps the original task + last 4 messages, summarizes the middle via LLM.

---

## Spec drift

When SPEC.md changes after tasks have been generated, you have two options:

**Full regeneration** (`specr tasks`) — regenerates all open/failed tasks from the current spec. Done tasks are preserved.

**Incremental drift** (`specr drift`) — generates only new tasks for what changed:

```bash
specr drift              # auto-detects the right base (last SPEC.md commit)
specr drift --base v1.0  # explicit base ref
```

Auto-detection: finds the last commit that touched `SPEC.md` and diffs against its parent. Works whether your spec changes are committed or not.

New tasks are appended to `TASKS.md` with IDs continuing from the highest existing ID. Existing tasks are untouched.

---

## Agent instructions

Customize what each agent does for your project by adding instruction files:

```
.specr/
  coder.md          ← coding agent rules
  code-reviewer.md  ← review focus / pass criteria
  qa-reviewer.md    ← coverage target, test framework
  style-reviewer.md ← naming, formatting conventions
  coordinator.md    ← conflict resolution preferences
```

Instructions are appended to each agent's system prompt. Global defaults live in `~/.config/specr/instructions/` and apply to all projects.

```bash
specr instructions init      # scaffold with commented templates
specr instructions generate  # LLM generates from your SPEC.md
specr instructions show      # what's active right now
```

**Per-task instructions** — add an `## Agent Instructions` section to any task detail file. That content is appended to the coder prompt for that task only:

```markdown
## Agent Instructions
Do not modify the existing proto file. Only add new message types.
```

Priority: global → project (`.specr/`) → per-task (additive, not override).

---

## Configuration

Config file: `~/Library/Application Support/specr/config.toml` (macOS).

```toml
[llm]
provider = "claude-cli"           # claude-cli | anthropic | openai
model = "sonnet"                  # alias or full model ID
api_key_env = "CLAUDE_CODE_OAUTH_TOKEN"
timeout_seconds = 300
review_timeout_seconds = 120
review_model = ""                 # separate model for reviews (e.g. "claude-haiku-4-5-20251001")
review_provider = ""              # separate provider for reviews
review_api_key_env = ""           # separate key env var for reviews

[output]
base_dir = "."
obsidian_dir = ""                 # optional: mirror SPEC.md to Obsidian vault

[spec]
question_budget = 8               # LLM-generated extra questions per compose session
max_loop_iterations = 10          # review loop iterations before task fails

[agent]
runner = "claude-code"            # claude-code | api-agent
runner_bin = "claude"
stream_output = true
show_reasoning = true             # show LLM plan before coding each iteration
max_agent_turns = 30              # api-agent only: max tool calls per task
parallel_jobs = 1                 # >1 enables coordinator parallel mode
worktree_base = ""                # OS temp dir by default
test_command = ""                 # auto-detected (cargo test, npm test, make test, pytest)
resolve_conflicts = true          # auto-resolve merge conflicts with LLM

[telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"
chat_id_env = "TELEGRAM_CHAT_ID"
enabled = false
autostart = false
```

### LLM providers

**`claude-cli`** (default) — `claude` CLI subprocess. No API key needed.

**`anthropic`** — direct Anthropic Messages API. Set `CLAUDE_CODE_OAUTH_TOKEN` (OAuth) or `ANTHROPIC_API_KEY`.

**`openai`** — OpenAI Chat Completions. Set `OPENAI_API_KEY`.

---

## Telegram bot

When `telegram.enabled = true`, `specr bot` starts a polling loop. Supported messages:

```
compose a spec for <idea>
generate tasks
run next task
run 003
status
```

Progress updates and failures are sent back as Telegram messages. CLI and bot share the same on-disk state.

---

## Project layout

```
src/
├── main.rs                    # CLI entry + command dispatch
├── config.rs                  # Config loading (TOML + env vars)
├── types.rs                   # Task, TaskStatus, TaskSize, Finding, etc.
├── store.rs                   # Read/write SPEC.md, TASKS.md, task detail files
├── instructions.rs            # Agent instruction loading (.specr/ + global)
├── llm/
│   ├── mod.rs                 # LlmClient trait (Send + Sync)
│   ├── anthropic.rs           # Anthropic Messages API (OAuth + API key)
│   ├── claude_cli.rs          # Claude Code CLI subprocess
│   └── openai.rs              # OpenAI Chat Completions
├── spec_composer/
│   ├── mod.rs                 # Q&A pipeline + editor review loop
│   ├── questions.rs           # Seed questions + LLM-generated extras
│   └── renderer.rs            # SPEC.md template rendering
├── task_generator/
│   ├── mod.rs                 # Decomposition + drift pipeline
│   ├── graph.rs               # Dependency graph + parallel-safe grouping
│   └── renderer.rs            # TASKS.md + task file rendering
├── agent_runner/
│   ├── mod.rs                 # Task execution orchestration
│   ├── resolver.rs            # Dependency resolver + parallel groups
│   ├── coding_agent.rs        # Claude Code subprocess + prompt builder
│   ├── api_agent.rs           # Direct Anthropic tool-use agent
│   ├── coordinator.rs         # Parallel coordinator + worktree + conflict resolution
│   ├── review.rs              # Parallel review LLM calls (code/QA/style)
│   └── loop_controller.rs     # Iteration counter + pass/fail logic
├── tui/
│   └── mod.rs                 # ratatui task board
└── telegram/
    ├── mod.rs                 # Bot setup + message routing
    └── notify.rs              # Progress notifications
```

---

## Task sizing

| Size | Guideline | Behaviour |
|---|---|---|
| S | < 2 hours | Assigned directly |
| M | ~half day | Assigned with detail file |
| L | > half day | Blocked — must run `specr split --task NNN` first |

---

## License

MIT
