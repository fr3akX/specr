/// Parallel task coordinator.
///
/// Runs multiple coding workers concurrently, each in their own git worktree.
/// After a worker finishes, the coordinator merges its branch into the main
/// workdir, runs the project's test command, and (optionally) uses the LLM to
/// resolve conflicts or failing tests before marking the task done/failed.
///
/// Architecture:
///   - Coordinator owns the main workdir (stays on default branch).
///   - Each worker gets a fresh `git worktree add` at `<worktree_base>/<task-id>`.
///   - Workers run the full code → review pipeline and report back via mpsc channel.
///   - TASKS.md is only written by the coordinator (workers skip it).
///   - Merging is serialized: one branch at a time, tests run after each merge.
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result};
use colored::Colorize;
use tokio::sync::{mpsc, Semaphore};

use crate::config::Config;
use crate::llm::LlmClient;
use crate::store;
use crate::telegram::TelegramNotifier;
use crate::types::{Task, TaskStatus};

use super::coding_agent::git_command;

// ── System prompt for LLM conflict / test-failure resolution ───────────────

const CONFLICT_RESOLVE_SYSTEM: &str = "\
You are a senior software engineer resolving a git merge conflict. You will be given \
the conflicted file content (with <<<<<<< / ======= / >>>>>>> markers), the scopes of \
both tasks involved, and failing test output. Output ONLY the fully resolved file \
content — no explanation, no markdown fences, just the raw file text that makes all \
tests pass.";

const TEST_FIX_SYSTEM: &str = "\
You are a senior software engineer. Integration tests are failing after merging a task branch. \
You will be given the failing test output and the relevant source files. Identify the root cause \
and output a JSON array of file patches in this format:
[{\"path\": \"src/foo.rs\", \"content\": \"<complete new file content>\"}]
Output JSON only, no commentary.";

// ── Message types ───────────────────────────────────────────────────────────

#[derive(Debug)]
pub enum WorkerMsg {
    Done { task_id: String, branch: String },
    Failed { task_id: String, reason: String },
}

// ── Worktree helpers ────────────────────────────────────────────────────────

/// Absolute path where a task's worktree will be created.
pub fn worktree_path(base: &Path, task_id: &str) -> PathBuf {
    base.join(format!("specr-worker-{task_id}"))
}

/// `git worktree add <path> -b <branch>` (creates the branch if needed).
/// If the branch already exists, adds the worktree pointing at it.
pub async fn worktree_add(repo_dir: &Path, path: &Path, branch: &str) -> Result<()> {
    // Remove stale worktree dir if it exists from a previous run
    if path.exists() {
        tokio::fs::remove_dir_all(path).await.ok();
        git_command(repo_dir, &["worktree", "prune"]).await.ok();
    }

    // Check if branch already exists
    let branch_exists = git_command(repo_dir, &["rev-parse", "--verify", branch])
        .await
        .is_ok();

    if branch_exists {
        git_command(
            repo_dir,
            &["worktree", "add", &path.to_string_lossy(), branch],
        )
        .await?;
    } else {
        git_command(
            repo_dir,
            &["worktree", "add", "-b", branch, &path.to_string_lossy()],
        )
        .await?;
    }
    Ok(())
}

/// `git worktree remove --force <path>` — removes the worktree.
pub async fn worktree_remove(repo_dir: &Path, path: &Path) -> Result<()> {
    git_command(
        repo_dir,
        &["worktree", "remove", "--force", &path.to_string_lossy()],
    )
    .await
    .ok();
    // prune stale entries
    git_command(repo_dir, &["worktree", "prune"]).await.ok();
    Ok(())
}

// ── Test command ────────────────────────────────────────────────────────────

/// Infer the test command from project files if `config.agent.test_command` is empty.
pub fn effective_test_command(config: &Config, dir: &Path) -> String {
    if !config.agent.test_command.is_empty() {
        return config.agent.test_command.clone();
    }
    if dir.join("Cargo.toml").exists() {
        return "cargo test".to_string();
    }
    if dir.join("package.json").exists() {
        return "npm test".to_string();
    }
    if dir.join("Makefile").exists() {
        return "make test".to_string();
    }
    if dir.join("setup.py").exists() || dir.join("pytest.ini").exists() {
        return "pytest".to_string();
    }
    "cargo test".to_string()
}

/// Run the test command in `dir`. Returns Ok if exit code is 0.
pub async fn run_test_command(dir: &Path, cmd: &str) -> Result<String> {
    let parts: Vec<&str> = cmd.split_whitespace().collect();
    if parts.is_empty() {
        anyhow::bail!("Empty test command");
    }
    let output = tokio::process::Command::new(parts[0])
        .args(&parts[1..])
        .current_dir(dir)
        .output()
        .await
        .with_context(|| format!("Failed to run test command: {cmd}"))?;

    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    let combined = format!("{stdout}\n{stderr}");

    if output.status.success() {
        Ok(combined)
    } else {
        anyhow::bail!("Tests failed:\n{combined}")
    }
}

// ── Merge helpers ───────────────────────────────────────────────────────────

/// Attempt a `git merge --no-ff <branch>` in `dir`.
/// Returns `Ok(true)` if merged cleanly, `Ok(false)` if there are conflicts.
pub async fn coordinator_merge(dir: &Path, branch: &str, default_branch: &str) -> Result<bool> {
    // Make sure we're on the default branch
    git_command(dir, &["checkout", default_branch]).await?;

    let result = git_command(dir, &["merge", "--no-ff", "--no-edit", branch]).await;
    match result {
        Ok(_) => Ok(true),
        Err(e) => {
            let msg = e.to_string();
            if msg.contains("CONFLICT") || msg.contains("conflict") {
                Ok(false) // conflict — caller decides what to do
            } else {
                Err(e)
            }
        }
    }
}

/// Collect files with conflict markers after a failed merge.
async fn get_conflicted_files(dir: &Path) -> Result<Vec<String>> {
    let out = git_command(dir, &["diff", "--name-only", "--diff-filter=U"]).await?;
    Ok(out
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect())
}

// ── LLM conflict resolution ─────────────────────────────────────────────────

/// Resolve merge conflicts in `dir` using the LLM.
/// Reads each conflicted file, asks LLM to resolve, writes result, then runs tests.
/// Returns Ok(true) if resolution succeeded and tests pass.
async fn resolve_conflicts(dir: &Path, task: &Task, llm: &dyn LlmClient, test_cmd: &str) -> bool {
    let conflicted = match get_conflicted_files(dir).await {
        Ok(f) => f,
        Err(e) => {
            eprintln!("{} Failed to list conflicted files: {e}", "⚠".yellow());
            return false;
        }
    };

    if conflicted.is_empty() {
        // No conflict markers but tests failed — try test-fix path
        return fix_failing_tests(dir, task, llm, test_cmd).await;
    }

    eprintln!(
        "{} Resolving {} conflict(s) with LLM...",
        "⚙".cyan(),
        conflicted.len()
    );

    for file_path in &conflicted {
        let full_path = dir.join(file_path);
        let content = match std::fs::read_to_string(&full_path) {
            Ok(c) => c,
            Err(e) => {
                eprintln!("{} Cannot read {file_path}: {e}", "⚠".yellow());
                return false;
            }
        };

        let coord_instructions =
            crate::instructions::load(dir, crate::instructions::AgentKind::Coordinator);
        let conflict_system = format!(
            "{CONFLICT_RESOLVE_SYSTEM}{}",
            crate::instructions::as_system_appendix(&coord_instructions)
        );

        let user_prompt = format!(
            "Task scope: {}\n\nConflicted file: {file_path}\n\n{content}",
            task.scope
        );

        let resolved = match llm.complete(&conflict_system, &user_prompt).await {
            Ok(r) => r,
            Err(e) => {
                eprintln!("{} LLM conflict resolution failed: {e}", "⚠".yellow());
                return false;
            }
        };

        if std::fs::write(&full_path, &resolved).is_err() {
            return false;
        }
    }

    // Stage resolved files and complete the merge
    git_command(dir, &["add", "-u"]).await.ok();
    git_command(
        dir,
        &["commit", "--no-edit", "-m", "merge: LLM-resolved conflicts"],
    )
    .await
    .ok();

    // Run tests
    match run_test_command(dir, test_cmd).await {
        Ok(_) => {
            eprintln!("{} Conflict resolution: tests pass ✔", "✔".green());
            true
        }
        Err(e) => {
            eprintln!(
                "{} Tests still failing after conflict resolution: {e}",
                "✖".red()
            );
            false
        }
    }
}

/// Try to fix failing integration tests using the LLM.
async fn fix_failing_tests(dir: &Path, task: &Task, llm: &dyn LlmClient, test_cmd: &str) -> bool {
    let test_result = run_test_command(dir, test_cmd).await;
    let test_output = match test_result {
        Ok(_) => return true, // already passing
        Err(e) => e.to_string(),
    };

    eprintln!("{} Attempting LLM test-failure fix...", "⚙".cyan());

    // Collect relevant files (files_to_touch from the task spec)
    let mut file_context = String::new();
    for rel_path in &task.files_to_touch {
        let full = dir.join(rel_path);
        if let Ok(content) = std::fs::read_to_string(&full) {
            file_context.push_str(&format!("\n\n// {rel_path}\n{content}"));
        }
    }

    let coord_instructions =
        crate::instructions::load(dir, crate::instructions::AgentKind::Coordinator);
    let test_fix_system = format!(
        "{TEST_FIX_SYSTEM}{}",
        crate::instructions::as_system_appendix(&coord_instructions)
    );

    let user_prompt = format!(
        "Task: {} — {}\nScope: {}\n\nFailing tests:\n{test_output}\n\nRelevant files:{file_context}",
        task.id, task.name, task.scope
    );

    let response = match llm.complete(&test_fix_system, &user_prompt).await {
        Ok(r) => r,
        Err(e) => {
            eprintln!("{} LLM test-fix call failed: {e}", "⚠".yellow());
            return false;
        }
    };

    // Parse JSON patches
    #[derive(serde::Deserialize)]
    struct Patch {
        path: String,
        content: String,
    }

    let trimmed = {
        let t = response.trim();
        if let (Some(s), Some(e)) = (t.find('['), t.rfind(']')) {
            t[s..=e].to_string()
        } else {
            t.to_string()
        }
    };

    let patches: Vec<Patch> = match serde_json::from_str(&trimmed) {
        Ok(p) => p,
        Err(e) => {
            eprintln!("{} Failed to parse LLM patches: {e}", "⚠".yellow());
            return false;
        }
    };

    for patch in &patches {
        let full = dir.join(&patch.path);
        if std::fs::write(&full, &patch.content).is_err() {
            return false;
        }
    }

    git_command(dir, &["add", "-u"]).await.ok();
    git_command(dir, &["commit", "-m", "fix: LLM integration test fix"])
        .await
        .ok();

    run_test_command(dir, test_cmd).await.is_ok()
}

// ── Shared coordinator context ───────────────────────────────────────────────

/// Owned context shared between the coordinator loop and spawned workers.
/// All fields are cheaply cloneable (Arc or Clone).
#[derive(Clone)]
#[allow(dead_code)]
pub struct CoordCtx {
    pub config: Arc<Config>,
    pub llm: Arc<dyn LlmClient>,
    pub review_llm: Arc<dyn LlmClient>,
    pub notifier: TelegramNotifier,
    pub workdir: PathBuf,
    pub spec_content: String,
    pub default_branch: String,
    pub test_cmd: String,
}

// ── Worker ──────────────────────────────────────────────────────────────────

/// Run a single task in its worktree. Reports result via `tx`.
/// Does NOT write TASKS.md — coordinator manages that.
async fn run_worker(ctx: CoordCtx, worktree: PathBuf, task: Task, tx: mpsc::Sender<WorkerMsg>) {
    let task_id = task.id.clone();
    let branch = task.branch.clone();

    let result = super::run_single_task_in_worktree(
        &ctx.config,
        ctx.llm.as_ref(),
        ctx.review_llm.as_ref(),
        &ctx.notifier,
        &task,
        &ctx.spec_content,
        &worktree,
        &ctx.default_branch,
    )
    .await;

    let msg = match result {
        Ok(()) => WorkerMsg::Done { task_id, branch },
        Err(e) => WorkerMsg::Failed {
            task_id,
            reason: e.to_string(),
        },
    };

    tx.send(msg).await.ok();
}

// ── Coordinator main loop ───────────────────────────────────────────────────

#[allow(clippy::too_many_arguments)]
pub async fn run_parallel(
    config: Arc<Config>,
    llm: Arc<dyn LlmClient>,
    review_llm: Arc<dyn LlmClient>,
    notifier: TelegramNotifier,
    workdir: PathBuf,
    spec_content: String,
    max_jobs: usize,
    default_branch: String,
) -> Result<()> {
    let test_cmd = effective_test_command(&config, &workdir);
    let worktree_base = PathBuf::from(if config.agent.worktree_base.is_empty() {
        std::env::temp_dir().to_string_lossy().into_owned()
    } else {
        config.agent.worktree_base.clone()
    });

    let cctx = CoordCtx {
        config: Arc::clone(&config),
        llm: Arc::clone(&llm),
        review_llm: Arc::clone(&review_llm),
        notifier: notifier.clone(),
        workdir: workdir.clone(),
        spec_content: spec_content.clone(),
        default_branch: default_branch.clone(),
        test_cmd: test_cmd.clone(),
    };
    // Convenient local refs into cctx so the coordinator body reads naturally
    let llm = cctx.llm.as_ref();
    let notifier = &cctx.notifier;
    let config = cctx.config.as_ref();

    println!(
        "\n{} Parallel mode: max {} jobs  test: {}\n",
        "⚡".bold().yellow(),
        max_jobs,
        test_cmd.dimmed()
    );

    let (tx, mut rx) = mpsc::channel::<WorkerMsg>(64);
    let sem = Arc::new(Semaphore::new(max_jobs));

    // Load initial task state
    let (all_tasks, _) = store::read_tasks(&workdir)?;
    let mut task_map: HashMap<String, Task> =
        all_tasks.into_iter().map(|t| (t.id.clone(), t)).collect();

    let mut done_ids: HashSet<String> = task_map
        .values()
        .filter(|t| t.status == TaskStatus::Done)
        .map(|t| t.id.clone())
        .collect();

    let mut in_flight: HashSet<String> = HashSet::new();
    let mut failed_ids: HashSet<String> = HashSet::new();

    loop {
        // Find eligible tasks: open, deps done, not in-flight
        let eligible: Vec<Task> = task_map
            .values()
            .filter(|t| {
                t.status == TaskStatus::Open
                    && !in_flight.contains(&t.id)
                    && !failed_ids.contains(&t.id)
                    && t.depends_on.iter().all(|dep| done_ids.contains(dep))
            })
            .cloned()
            .collect();

        // Spawn up to max_jobs workers
        for task in eligible {
            if in_flight.len() >= max_jobs {
                break;
            }

            let permit = match Arc::clone(&sem).try_acquire_owned() {
                Ok(p) => p,
                Err(_) => break,
            };

            let worktree = worktree_path(&worktree_base, &task.id);
            println!(
                "{} [{}] {} → worktree {}",
                "▶".bold().cyan(),
                task.id.bold(),
                task.name,
                worktree.display()
            );

            // Mark in-progress in TASKS.md (coordinator does this)
            store::update_task_status(&workdir, &task.id, TaskStatus::InProgress)?;
            git_command(&workdir, &["add", "TASKS.md"]).await.ok();
            git_command(
                &workdir,
                &[
                    "commit",
                    "-m",
                    &format!("chore: mark task {} as in-progress [parallel]", task.id),
                ],
            )
            .await
            .ok();

            // Setup worktree
            if let Err(e) = worktree_add(&workdir, &worktree, &task.branch).await {
                eprintln!(
                    "{} Failed to create worktree for task {}: {e}",
                    "✖".red(),
                    task.id
                );
                store::update_task_status(&workdir, &task.id, TaskStatus::Failed)?;
                failed_ids.insert(task.id.clone());
                continue;
            }

            in_flight.insert(task.id.clone());
            if let Some(t) = task_map.get_mut(&task.id) {
                t.status = TaskStatus::InProgress;
            }

            let worker_ctx = cctx.clone();
            let tx_c = tx.clone();
            let workdir_c = workdir.clone();

            tokio::spawn(async move {
                run_worker(worker_ctx, worktree.clone(), task, tx_c).await;
                // Cleanup worktree regardless of outcome
                worktree_remove(&workdir_c, &worktree).await.ok();
                drop(permit);
            });
        }

        // Check termination: nothing in-flight and nothing left to start
        let open_count = task_map
            .values()
            .filter(|t| t.status == TaskStatus::Open)
            .count();
        let done_count = done_ids.len();
        let total = task_map.len();

        if in_flight.is_empty() && open_count == 0 {
            let failed = failed_ids.len();
            if failed == 0 {
                println!(
                    "\n{} All {} tasks completed!\n",
                    "✔".bold().green(),
                    done_count
                );
                notifier
                    .send(&format!("✅ All {} tasks completed!", done_count))
                    .await
                    .ok();
            } else {
                println!(
                    "\n{} Done: {}/{} tasks completed, {} failed\n",
                    "ℹ".bold().blue(),
                    done_count,
                    total,
                    failed
                );
                notifier
                    .send(&format!(
                        "⚠️ {}/{} tasks done, {} failed",
                        done_count, total, failed
                    ))
                    .await
                    .ok();
            }
            return Ok(());
        }

        // All in-flight or blocked by failures
        if in_flight.is_empty() {
            println!(
                "\n{} No more tasks can start (blocked by dependencies or failures). {}/{} done.\n",
                "ℹ".bold().blue(),
                done_count,
                total
            );
            return Ok(());
        }

        // Wait for the next worker to finish
        let msg = match rx.recv().await {
            Some(m) => m,
            None => break,
        };

        match msg {
            WorkerMsg::Done { task_id, branch } => {
                in_flight.remove(&task_id);

                let task = task_map
                    .get(&task_id)
                    .cloned()
                    .unwrap_or_else(|| Task::default_with_id(&task_id));

                println!(
                    "\n{} [{}] Worker done — merging branch {}",
                    "⎇".bold(),
                    task_id.bold(),
                    branch.cyan()
                );

                // Try to merge the task branch
                let merged = coordinator_merge(&workdir, &branch, &default_branch).await?;

                if merged {
                    // Run tests to confirm integration
                    println!("{} Running tests after merge...", "⧗".dimmed());
                    match run_test_command(&workdir, &test_cmd).await {
                        Ok(_) => {
                            println!(
                                "{} [{}] Tests pass — task done ✔",
                                "✔".bold().green(),
                                task_id.bold()
                            );
                            finalize_task_done(
                                &workdir,
                                &task_id,
                                &branch,
                                notifier,
                                &default_branch,
                                &task_map,
                            )
                            .await;
                            done_ids.insert(task_id.clone());
                            if let Some(t) = task_map.get_mut(&task_id) {
                                t.status = TaskStatus::Done;
                            }
                            store::update_task_status(&workdir, &task_id, TaskStatus::Done)?;
                            push_tasks_md(&workdir, &task_id, "done").await;
                        }
                        Err(test_err) => {
                            println!(
                                "{} [{}] Tests failed after merge: {}",
                                "✖".bold().red(),
                                task_id.bold(),
                                test_err
                            );

                            if config.agent.resolve_conflicts {
                                println!("{} Attempting LLM test-fix...", "⚙".cyan());
                                if fix_failing_tests(&workdir, &task, llm, &test_cmd).await {
                                    println!(
                                        "{} [{}] LLM fix: tests pass ✔",
                                        "✔".green(),
                                        task_id.bold()
                                    );
                                    done_ids.insert(task_id.clone());
                                    if let Some(t) = task_map.get_mut(&task_id) {
                                        t.status = TaskStatus::Done;
                                    }
                                    store::update_task_status(
                                        &workdir,
                                        &task_id,
                                        TaskStatus::Done,
                                    )?;
                                    push_tasks_md(&workdir, &task_id, "done").await;
                                } else {
                                    revert_merge(&workdir, &default_branch).await;
                                    mark_failed(
                                        &workdir,
                                        &task_id,
                                        notifier,
                                        &mut task_map,
                                        &mut failed_ids,
                                        "tests failed after merge + LLM fix",
                                    )
                                    .await;
                                }
                            } else {
                                revert_merge(&workdir, &default_branch).await;
                                mark_failed(
                                    &workdir,
                                    &task_id,
                                    notifier,
                                    &mut task_map,
                                    &mut failed_ids,
                                    "tests failed after merge",
                                )
                                .await;
                            }
                        }
                    }
                } else {
                    // Merge conflict
                    println!(
                        "{} [{}] Merge conflict detected",
                        "⚠".yellow(),
                        task_id.bold()
                    );

                    if config.agent.resolve_conflicts {
                        if resolve_conflicts(&workdir, &task, llm, &test_cmd).await {
                            println!(
                                "{} [{}] Conflict resolved: tests pass ✔",
                                "✔".green(),
                                task_id.bold()
                            );
                            done_ids.insert(task_id.clone());
                            if let Some(t) = task_map.get_mut(&task_id) {
                                t.status = TaskStatus::Done;
                            }
                            store::update_task_status(&workdir, &task_id, TaskStatus::Done)?;
                            push_tasks_md(&workdir, &task_id, "done").await;
                        } else {
                            git_command(&workdir, &["merge", "--abort"]).await.ok();
                            mark_failed(
                                &workdir,
                                &task_id,
                                notifier,
                                &mut task_map,
                                &mut failed_ids,
                                "merge conflict, LLM resolution failed",
                            )
                            .await;
                        }
                    } else {
                        git_command(&workdir, &["merge", "--abort"]).await.ok();
                        mark_failed(
                            &workdir,
                            &task_id,
                            notifier,
                            &mut task_map,
                            &mut failed_ids,
                            "merge conflict (resolve_conflicts disabled)",
                        )
                        .await;
                    }
                }
            }

            WorkerMsg::Failed { task_id, reason } => {
                in_flight.remove(&task_id);
                eprintln!(
                    "{} [{}] Worker failed: {}",
                    "✖".bold().red(),
                    task_id.bold(),
                    reason
                );
                mark_failed(
                    &workdir,
                    &task_id,
                    notifier,
                    &mut task_map,
                    &mut failed_ids,
                    &reason,
                )
                .await;
            }
        }
    }

    Ok(())
}

// ── Helpers ─────────────────────────────────────────────────────────────────

async fn finalize_task_done(
    workdir: &Path,
    task_id: &str,
    branch: &str,
    notifier: &TelegramNotifier,
    default_branch: &str,
    task_map: &HashMap<String, Task>,
) {
    let name = task_map
        .get(task_id)
        .map(|t| t.name.as_str())
        .unwrap_or("?");
    notifier
        .send(&format!("✅ Task {task_id} done: {name}"))
        .await
        .ok();
    // Push task branch for reference
    git_command(workdir, &["push", "origin", branch]).await.ok();
    git_command(workdir, &["push", "origin", default_branch])
        .await
        .ok();
}

async fn push_tasks_md(workdir: &Path, task_id: &str, status: &str) {
    git_command(workdir, &["add", "TASKS.md"]).await.ok();
    git_command(
        workdir,
        &[
            "commit",
            "-m",
            &format!("chore: mark task {task_id} as {status} [coordinator]"),
        ],
    )
    .await
    .ok();
}

async fn revert_merge(workdir: &Path, default_branch: &str) {
    // Reset the last commit (the merge commit)
    git_command(
        workdir,
        &["reset", "--hard", &format!("{default_branch}@{{1}}")],
    )
    .await
    .ok();
}

async fn mark_failed(
    workdir: &Path,
    task_id: &str,
    notifier: &TelegramNotifier,
    task_map: &mut HashMap<String, Task>,
    failed_ids: &mut HashSet<String>,
    reason: &str,
) {
    store::update_task_status(workdir, task_id, TaskStatus::Failed).ok();
    push_tasks_md(workdir, task_id, "failed").await;
    failed_ids.insert(task_id.to_string());
    if let Some(t) = task_map.get_mut(task_id) {
        t.status = TaskStatus::Failed;
    }
    notifier
        .send(&format!("❌ Task {task_id} failed: {reason}"))
        .await
        .ok();
}

// ── Tests ────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_worktree_path_contains_task_id() {
        let base = Path::new("/tmp/specr");
        let p = worktree_path(base, "007");
        assert!(p.to_string_lossy().contains("007"));
        assert!(p.to_string_lossy().contains("specr-worker"));
    }

    #[test]
    fn test_effective_test_command_cargo() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let config = Config::default();
        let cmd = effective_test_command(&config, dir);
        assert_eq!(cmd, "cargo test");
    }

    #[test]
    fn test_effective_test_command_override() {
        let mut config = Config::default();
        config.agent.test_command = "make test".to_string();
        let cmd = effective_test_command(&config, Path::new("/any"));
        assert_eq!(cmd, "make test");
    }

    #[test]
    fn test_effective_test_command_fallback() {
        let config = Config::default();
        // Path with no known project files → falls back to cargo test
        let cmd = effective_test_command(&config, Path::new("/tmp"));
        assert_eq!(cmd, "cargo test");
    }

    #[tokio::test]
    async fn test_run_test_command_success() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let result = run_test_command(dir, "echo ok").await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_run_test_command_failure() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let result = run_test_command(dir, "false").await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn test_run_test_command_bad_binary() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let result = run_test_command(dir, "__no_such_binary__").await;
        assert!(result.is_err());
    }

    #[test]
    fn test_conflict_resolve_system_has_key_phrases() {
        assert!(CONFLICT_RESOLVE_SYSTEM.contains("conflict"));
        assert!(CONFLICT_RESOLVE_SYSTEM.contains("resolved file content"));
    }

    #[test]
    fn test_test_fix_system_has_key_phrases() {
        assert!(TEST_FIX_SYSTEM.contains("JSON array"));
        assert!(TEST_FIX_SYSTEM.contains("path"));
        assert!(TEST_FIX_SYSTEM.contains("content"));
    }
}
