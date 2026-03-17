# specr

Turn a rough idea into a structured, machine-readable project spec and task breakdown — then drive agentic development through a quality-gated execution loop.

```
specr compose "a REST API for managing cycling workouts"
```

---

## What it does

**specr** is a three-phase pipeline:

1. **spec-composer** — guided Q&A turns your idea into a reviewed `SPEC.md`
2. **task-generator** — decomposes `SPEC.md` into a sized, dependency-ordered task list
3. **agent-runner** — executes tasks via Claude Code, runs parallel code/QA/style reviews, loops until clean

The output is three layers:

```
SPEC.md              ← the contract (what & why)
TASKS.md             ← ordered task list with sizes and dependencies
tasks/NNN-name.md    ← per-task detail files (required for M and L tasks)
```

---

## Install

```bash
cargo install --path .
```

Requires Rust 1.75+ and the `claude` CLI (Claude Code) in your PATH for the agent-runner.

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
```

No API key needed by default — uses your Claude Code subscription via the `claude` CLI.

---

## Commands

| Command | Description |
|---------|-------------|
| `specr compose "<idea>"` | Guided Q&A → draft SPEC.md → editor review → write |
| `specr refine` | Re-open existing SPEC.md for editing, bumps spec-version |
| `specr tasks` | Decompose SPEC.md into TASKS.md + task detail files |
| `specr status` | Interactive ratatui TUI task board |
| `specr split --task NNN` | Split an L-sized task into subtasks |
| `specr run` | Run next eligible task(s) through the full review pipeline |
| `specr run --task NNN` | Run a specific task by ID |
| `specr bot` | Start Telegram bot daemon |

---

## How the spec-composer works

`specr compose` is a two-step Q&A + draft pipeline:

### Step 1 — Questions

specr asks two rounds of questions:

**Predefined seed questions** (always asked, cover the universal baseline):
1. What language/runtime and framework should be used?
2. What is the deployment target?
3. What is explicitly out of scope for v1?
4. What are the main data entities and their relationships?
5. What auth or security requirements exist (if any)?

**LLM-generated extras** — the LLM receives the seeds as context ("already covered") and generates project-specific questions on top. A cycling tracker gets questions about GPX/FIT import and power data. An invoice API gets questions about PDF generation and multi-currency. No overlap, no boilerplate.

All questions are presented one at a time. Press Enter to skip any.

### Step 2 — Draft + editor review

After answering, specr generates a SPEC.md draft and presents it in the terminal:

```
Approve? [y]es / [e]dit in $EDITOR / [n]o
```

- **`y`** — write SPEC.md to disk. Done.
- **`n`** — abort, nothing written.
- **`e`** — opens the draft in `$EDITOR`. Edit freely: rewrite sections, add `<!-- inline comments -->`, annotate what needs changing. On close, specr feeds your edits back to the LLM which produces a revised draft. Loop until satisfied.

Set your preferred editor with `export EDITOR=vim` (or `code --wait` for VS Code, `nano` is the fallback).

---

## How the agent-runner works

```
specr run
  → dependency resolver picks eligible tasks (deps done, size S or M)
  → auto-detects default branch (main/master/etc.)
  → creates git branch: task/NNN-name
  → spawns Claude Code — output streamed to terminal
  → on completion:
      ┌─────────────────────────────────────────┐
      │  code review  │  QA review  │  style    │  ← parallel, diff only
      └─────────────────────────────────────────┘
      each returns: { verdict, critical, warnings, suggestions }
  → any critical finding → findings sent back to Claude Code → re-run
  → loop limit (default 5) → task marked FAILED, branch preserved
  → all pass → task marked DONE, branch merged
```

**Review agents** each get the git diff (not the full repo):
- **Code review**: correctness, spec compliance, security, error handling
- **QA review**: tests cover behaviour (not just lines), edge cases, meaningful assertions
- **Style review**: clarity, simplification, naming

Only `critical` findings block task completion. `warnings` and `suggestions` are logged.

---

## Task sizing

| Size | Guideline | Action |
|------|-----------|--------|
| S | < 2 hours | Assign directly |
| M | ~half day | Assign with detail file |
| L | > half day | Must be split before running |

L tasks are never handed to an agent. Use `specr split --task NNN` to break them down first.

---

## Spec drift

When you update `SPEC.md` after tasks have been generated:

- `done` tasks → preserved as-is
- `open` tasks → regenerated from new spec
- `in-progress` tasks → flagged for manual review
- `failed` tasks → re-evaluated, reset to open if still relevant

`spec-version` in frontmatter tracks this — a mismatch between SPEC.md and TASKS.md means drift has occurred.

---

## Configuration

Config file: `~/Library/Application Support/specr/config.toml` (macOS) — auto-created with defaults on first run.

```toml
[llm]
provider = "claude-cli"          # claude-cli | anthropic | openai
model = "sonnet"                 # model alias or full name
api_key_env = "CLAUDE_CODE_OAUTH_TOKEN"  # only needed for anthropic/openai providers
timeout_seconds = 300            # timeout for a single LLM completion

[output]
base_dir = "."                   # where SPEC.md / TASKS.md are written
obsidian_dir = ""                # optional: also write to Obsidian vault

[spec]
question_budget = 8              # target number of LLM-generated extra questions
max_loop_iterations = 5          # max review loop iterations before task fails

[agent]
runner = "claude-code"
runner_bin = "claude"            # path or name of Claude Code binary
stream_output = true             # stream coding agent output to terminal

[telegram]
bot_token_env = "TELEGRAM_BOT_TOKEN"
chat_id_env = "TELEGRAM_CHAT_ID"
enabled = false                  # set true to enable bot
autostart = false                # start bot daemon automatically
```

### LLM providers

**`claude-cli`** (default) — uses the `claude` CLI subprocess. No API key needed; uses your Claude Code subscription. Runs from a temp directory to avoid workspace scanning delays.

**`anthropic`** — direct Anthropic API calls. Set `CLAUDE_CODE_OAUTH_TOKEN`.

**`openai`** — OpenAI Chat Completions. Set `OPENAI_API_KEY` (and update `api_key_env`).

---

## Telegram bot

When `telegram.enabled = true`, `specr bot` starts a polling loop:

```
compose a spec for <idea>
generate tasks
run next task
run 003
status
```

Progress updates, completions, and failures are sent as Telegram messages. CLI and bot share the same on-disk state and can run simultaneously.

---

## Defaults

| Setting | Value |
|---------|-------|
| LLM provider | claude-cli (Claude Code subscription) |
| Test coverage | 90% unit minimum |
| Deployment target | Local binary |
| Dockerfile | Only when explicitly requested |
| Secrets in spec | Env var names only, never values |
| spec-version | Auto-incremented integer, starts at 1 |
| Seed questions | 5 fixed baseline questions |
| Extra question budget | 8 (LLM-generated, project-specific) |
| Max review iterations | 5, then task = failed |
| Git branch | `task/NNN-name` per task, auto-detected default branch |
| Editor fallback | `$EDITOR` → `$VISUAL` → `nano` |

---

## Project layout

```
src/
├── main.rs                  # CLI entry + command dispatch
├── config.rs                # Config loading (TOML + env vars)
├── types.rs                 # Shared types: Task, TaskStatus, Finding, etc.
├── store.rs                 # Read/write SPEC.md, TASKS.md, task files
├── llm/
│   ├── mod.rs               # LlmClient trait
│   ├── anthropic.rs         # Anthropic Messages API
│   ├── claude_cli.rs        # Claude Code CLI subprocess
│   └── openai.rs            # OpenAI Chat Completions
├── spec_composer/
│   ├── mod.rs               # Q&A pipeline + editor review loop
│   ├── questions.rs         # Seed questions + LLM-generated extras
│   └── renderer.rs          # SPEC.md template rendering
├── task_generator/
│   ├── mod.rs               # Decomposition pipeline
│   ├── graph.rs             # Dependency graph + parallel detection
│   └── renderer.rs          # TASKS.md + task file rendering
├── agent_runner/
│   ├── mod.rs               # Task execution orchestration
│   ├── resolver.rs          # Dependency resolver
│   ├── coding_agent.rs      # Claude Code subprocess
│   ├── review.rs            # Parallel review LLM calls
│   └── loop_controller.rs   # Iteration counter + pass/fail logic
├── tui/
│   └── mod.rs               # ratatui task board
└── telegram/
    ├── mod.rs               # Bot setup + message routing
    └── notify.rs            # Progress notifications
```

---

## License

MIT
