# specr — Phase 2: task-generator

Phase 1 (spec-composer) is complete and all tests pass. Now build Phase 2.

---

## Context

The existing codebase at `/Users/maris/Projects/specr` has:
- `src/types.rs` — shared types (TaskStatus, TaskSize, Task, Finding, etc.)
- `src/config.rs` — config loading
- `src/store.rs` — file I/O (read/write SPEC.md)
- `src/llm/` — LLM trait + Anthropic/OpenAI clients
- `src/spec_composer/` — Phase 1 spec-composer (complete)
- `src/main.rs` — CLI with clap (`compose`, `refine` commands)

Read the existing code before writing anything new. Extend, don't rewrite.

---

## Phase 2 scope — task-generator

Implement:
1. `specr tasks` — reads SPEC.md, decomposes into tasks via LLM, user reviews/approves, writes TASKS.md + task detail files
2. `specr status` — interactive ratatui TUI task board
3. `specr split --task NNN` — interactively split an L task into subtasks

---

## New files to add

```
src/
├── task_generator/
│   ├── mod.rs        # pipeline orchestration
│   ├── graph.rs      # dependency graph + parallel-safe detection
│   └── renderer.rs   # TASKS.md + task file rendering
└── tui/
    └── mod.rs        # ratatui task board
```

Update `src/main.rs` to add `tasks`, `status`, `split` subcommands.
Update `src/store.rs` to add TASKS.md + task file read/write.
Update `src/types.rs` if needed (Task, TaskSize, TaskStatus, etc.)

---

## TASKS.md format

```markdown
---
spec-version: 1
generated: YYYY-MM-DD
---

# Tasks — <Project Name>

## Backlog

- [ ] 001 · Scaffold project structure [S]
      Status: open
      Depends on: —
      Branch: task/001-scaffold
      Done when: repo exists, CI passes on empty project

- [ ] 002 · Implement data models [M]
      Status: open
      Depends on: 001
      Branch: task/002-data-models
      Done when: models defined, migrations run, unit tests green
      Detail: tasks/002-data-models.md

- [x] 003 · Implement auth module [L → split]
      Status: failed
      Depends on: 002
      Branch: task/003-auth
      Done when: N/A — split into subtasks
```

Status markers: `[ ]` = open/in-progress/failed, `[x]` = done

---

## Per-task detail file format (tasks/NNN-name.md)

Required for M and L tasks:

```markdown
# Task 002 · Implement data models

**Size:** M
**Status:** open
**Depends on:** 001
**Branch:** task/002-data-models
**Done when:** models defined, migrations run, unit tests green

## Scope
What exactly this task covers.

## Files to touch
- src/models/user.rs (create)
- src/models/post.rs (create)
- migrations/001_create_users.sql (create)

## Interface to implement
(function signatures, struct definitions, etc.)

## What NOT to change
(list of files/modules out of scope for this task)
```

---

## task_generator/mod.rs — pipeline

```
specr tasks
  1. Read SPEC.md from cwd
  2. Check spec-version — if TASKS.md exists and versions match, ask: regenerate? (y/n)
  3. Call LLM to decompose SPEC.md into tasks
  4. Build dependency graph (graph.rs)
  5. Size each task (S/M/L) — L tasks flagged
  6. Print task list for review
  7. Allow reordering/trimming (optional, user can skip with Enter)
  8. Approval gate: "Approve? (yes / edit / no)"
  9. On approval:
     - Write TASKS.md
     - Write tasks/NNN-name.md for all M and L tasks
     - Print summary
```

### Spec drift policy (if TASKS.md already exists with different spec-version):

- `done` tasks → preserved as-is
- `open` tasks → regenerated from new spec
- `in-progress` tasks → flagged in output, user must manually resolve
- `failed` tasks → re-evaluated, reset to open if still relevant
- Print clear diff summary before asking for approval

---

## task_generator/graph.rs — dependency graph

```rust
pub struct DependencyGraph {
    tasks: Vec<Task>,
}

impl DependencyGraph {
    pub fn build(tasks: Vec<Task>) -> Result<Self>
    pub fn validate(&self) -> Result<()>           // check for cycles
    pub fn eligible_tasks(&self) -> Vec<&Task>     // deps all done, status open
    pub fn parallel_safe(&self) -> Vec<Vec<&Task>> // groups that can run in parallel
}
```

---

## task_generator/renderer.rs

- `render_tasks_md(tasks: &[Task], spec_version: u32) -> String`
- `render_task_detail(task: &Task) -> String`
- Parse existing TASKS.md back into Vec<Task> (for drift detection)

---

## LLM prompt for task decomposition

System prompt:
```
You are a senior software architect. Given a SPEC.md, decompose the project into an ordered,
dependency-aware list of tasks for an agentic coding workflow.

Rules:
- Each task must produce ONE verifiable output
- Size tasks: S (<2h), M (~half day), L (>half day, must be split)
- Make dependencies explicit (task IDs)
- Done-when must be machine-checkable
- No more than 20 tasks for a typical project
- Output JSON only, no commentary
```

Output schema:
```json
[
  {
    "id": "001",
    "name": "Scaffold project structure",
    "size": "S",
    "depends_on": [],
    "done_when": "cargo build passes on empty project",
    "scope": "Create Cargo.toml, src/main.rs, .gitignore, CI config",
    "files_to_touch": ["Cargo.toml", "src/main.rs", ".github/workflows/ci.yml"],
    "not_to_change": []
  }
]
```

---

## tui/mod.rs — ratatui task board

`specr status` opens a full-screen TUI:

```
┌─ specr · Task Board ──────────────────────────────────────────────────────┐
│ Project: My Project                    spec-version: 2    2026-03-10      │
├────────────────────────────────────────────────────────────────────────────┤
│  ID   Task                            Size  Status      Depends on        │
│  001  Scaffold project structure       S    ✔ done      —                 │
│  002  Implement data models            M    ● in-prog   001               │
│  003  Implement auth module            L    ○ open      002               │
│  004  Add REST endpoints               M    ○ open      002               │
│  005  Write integration tests          S    ✖ failed    003,004           │
├────────────────────────────────────────────────────────────────────────────┤
│  [q] quit  [r] refresh  [↑↓] navigate  [enter] view detail  [s] split    │
└────────────────────────────────────────────────────────────────────────────┘
```

- `q` → quit
- `r` → refresh (re-read TASKS.md)
- `↑↓` → navigate rows
- `enter` → show task detail panel (reads tasks/NNN-name.md)
- `s` → split selected L task (triggers split flow)

Use `ratatui` + `crossterm`.

---

## specr split --task NNN

1. Read task NNN from TASKS.md — must be size L
2. Show current task scope
3. Ask LLM to suggest subtasks (S/M sized, same dependency chain)
4. Show suggested subtasks for review
5. User approves/edits
6. On approval: replace task NNN in TASKS.md with subtasks NNNa, NNNb, NNNc…
   (or new IDs if preferred — ask user)

---

## store.rs additions

- `read_tasks(dir: &Path) -> Result<Vec<Task>>`
- `write_tasks(dir: &Path, tasks: &[Task], spec_version: u32) -> Result<()>`
- `write_task_detail(dir: &Path, task: &Task) -> Result<()>`
- `read_task_detail(dir: &Path, task_id: &str) -> Result<String>`

---

## main.rs additions

```rust
// New subcommands:
Commands::Tasks => task_generator::run(&config).await,
Commands::Status => tui::run(&config),
Commands::Split { task } => task_generator::split(&config, &task).await,
```

---

## Code quality requirements

- 90% unit test coverage
- Tests for: dependency graph (cycle detection, eligible tasks, parallel groups),
  TASKS.md rendering + parsing, task detail rendering, drift detection logic
- No unwrap() in production paths
- cargo clippy clean

---

## Done when

- `specr tasks` runs end-to-end: reads SPEC.md, decomposes, writes TASKS.md + detail files on approval
- `specr status` opens ratatui TUI showing task board, navigable, detail panel works
- `specr split --task NNN` splits an L task into subtasks
- Spec drift detection works: existing done tasks preserved, open tasks regenerated
- `cargo test` passes with ≥90% coverage
- `cargo clippy` clean

---

When completely finished, run:
openclaw system event --text "Done: specr Phase 2 task-generator built" --mode now
