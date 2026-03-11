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
# Set your API key
export ANTHROPIC_API_KEY=sk-ant-...

# Compose a spec (interactive Q&A)
specr compose "a CLI tool to track cycling workouts"

# Decompose into tasks
specr tasks

# View task board
specr status

# Run next eligible task
specr run
```

---

## Commands

| Command | Description |
|---------|-------------|
| `specr compose "<idea>"` | Guided Q&A → draft SPEC.md → approve → write |
| `specr refine` | Re-open existing SPEC.md for editing, bumps spec-version |
| `specr tasks` | Decompose SPEC.md into TASKS.md + task detail files |
| `specr status` | Interactive ratatui TUI task board |
| `specr split --task NNN` | Split an L-sized task into subtasks |
| `specr run` | Run next eligible task(s) through the full review pipeline |
| `specr run --task NNN` | Run a specific task by ID |
| `specr bot` | Start Telegram bot daemon |

---

## Configuration

Config file: `~/.config/specr/config.toml` (auto-created with defaults on first run)

```toml
[llm]
provider = "claude-cli"          # claude-cli | anthropic | openai
model = ""                       # optional model override (empty = CLI default)
api_key_env = "ANTHROPIC_API_KEY"  # only needed for anthropic/openai providers

[output]
base_dir = "."                   # where SPEC.md / TASKS.md are written
obsidian_dir = ""                # optional: also write to Obsidian vault

[spec]
question_budget = 8              # max clarifying questions per session
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

All secrets are read from environment variables — never stored in the config file.

---

## How the spec-composer works

1. Trigger: `specr compose "your idea"`
2. specr asks clarifying questions **one at a time** (max 8), in priority order:
   - Goal → scope → stack → data models → API contracts → key workflows → constraints → open questions
3. Remaining unknowns are logged as assumptions in the spec's Open Questions section
4. specr drafts SPEC.md and presents it for review
5. You approve, edit, or abort — **nothing is written until you approve**
6. On approval: `SPEC.md` is written with frontmatter including `spec-version`

---

## How the agent-runner works

```
specr run
  → dependency resolver picks eligible tasks (deps done, size S or M)
  → creates git branch: task/NNN-name
  → spawns Claude Code — output streamed to terminal
  → on completion:
      ┌─────────────────────────────────────────┐
      │  code review  │  QA review  │  style    │  ← parallel, diff only
      └─────────────────────────────────────────┘
      each returns: { verdict, critical, warnings, suggestions }
  → any critical finding → findings sent back to Claude Code → re-run
  → loop limit (default 5) → task marked FAILED, branch preserved
  → all pass → task marked DONE, branch merged to main
```

**Review agents** each get the git diff (not the full repo), keeping context small and focused:
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

## Telegram bot

When `telegram.enabled = true`, `specr bot` starts a polling loop that accepts:

```
compose a spec for <idea>
generate tasks
run next task
run 003
status
```

Progress updates, task completions, and failures are sent as Telegram messages. CLI and bot share the same on-disk state and can run simultaneously.

---

## Defaults

| Setting | Value |
|---------|-------|
| Test coverage | 90% unit minimum |
| Deployment target | Local binary |
| Dockerfile | Only when explicitly requested |
| Secrets in spec | Env var names only, never values |
| spec-version | Auto-incremented integer, starts at 1 |
| Question budget | 8 max |
| Max review iterations | 5, then task = failed |

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
│   └── openai.rs            # OpenAI Chat Completions
├── spec_composer/
│   ├── mod.rs               # Q&A pipeline
│   ├── questions.rs         # Question set + priority order
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
