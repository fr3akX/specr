pub mod api_agent;
pub mod coding_agent;
pub mod loop_controller;
pub mod resolver;
pub mod review;

use anyhow::{Context, Result};
use colored::Colorize;
use std::path::Path;

use crate::config::Config;
use crate::llm::LlmClient;
use crate::store;
use crate::telegram::TelegramNotifier;
use crate::types::{Task, TaskStatus};

use coding_agent::{git_command, CodingAgent};
use loop_controller::LoopController;
use resolver::Resolver;
use review::run_reviews;

/// Detect the default branch name ("main", "master", or whatever HEAD points to).
async fn detect_default_branch(workdir: &Path) -> String {
    // Try: git symbolic-ref refs/remotes/origin/HEAD -> refs/remotes/origin/main
    if let Ok(out) = git_command(
        workdir,
        &["symbolic-ref", "--short", "refs/remotes/origin/HEAD"],
    )
    .await
    {
        let branch = out.trim().trim_start_matches("origin/").to_string();
        if !branch.is_empty() {
            return branch;
        }
    }

    // Fallback: check which of main/master exists locally
    for candidate in &["main", "master"] {
        if git_command(workdir, &["rev-parse", "--verify", candidate])
            .await
            .is_ok()
        {
            return candidate.to_string();
        }
    }

    // Last resort: use config value or hard default
    "main".to_string()
}

/// Run the agent pipeline for eligible (or specified) tasks.
///
/// - `task_id = Some(id)`: run one specific task, then exit.
// Shared execution context passed through the task pipeline to keep arg counts manageable.
struct RunCtx<'a> {
    config: &'a Config,
    llm: &'a dyn LlmClient,
    review_llm: &'a dyn LlmClient,
    notifier: &'a TelegramNotifier,
    default_branch: &'a str,
}

/// - `run_all = true`: loop through all parallel groups until all tasks done or a failure stops the run.
/// - Default: run the next parallel group, then exit.
pub async fn run(
    config: &Config,
    llm: &dyn LlmClient,
    review_llm: &dyn LlmClient,
    task_id: Option<&str>,
    run_all: bool,
) -> Result<()> {
    let workdir = std::env::current_dir()?;
    let spec_content = store::read_spec(&workdir)?;

    let notifier = TelegramNotifier::from_config(config);
    let default_branch = detect_default_branch(&workdir).await;
    println!(
        "{} Default branch: {}",
        "⎇".dimmed(),
        default_branch.dimmed()
    );

    let ctx = RunCtx {
        config,
        llm,
        review_llm,
        notifier: &notifier,
        default_branch: &default_branch,
    };

    // --task: run one specific task, ignore --all
    if let Some(id) = task_id {
        let (tasks, _) = store::read_tasks(&workdir)?;
        let task = tasks
            .iter()
            .find(|t| t.id == id)
            .with_context(|| format!("Task {} not found", id))?;
        if task.status != TaskStatus::Open {
            anyhow::bail!("Task {} has status '{}', expected 'open'", id, task.status);
        }
        println!(
            "\n{} Running task {}: {}\n",
            "→".bold(),
            task.id.bold(),
            task.name
        );
        return run_task_group(&ctx, &[task], &spec_content, &workdir).await;
    }

    // --all: loop until everything is done or a failure stops the run
    if run_all {
        return run_all_tasks(&ctx, &spec_content, &workdir).await;
    }

    // Default: run next parallel group, then stop
    let (tasks, _) = store::read_tasks(&workdir)?;
    let groups = Resolver::parallel_groups(&tasks);
    if groups.is_empty() {
        print_no_eligible_reason(&tasks);
        return Ok(());
    }
    let group = groups.into_iter().next().unwrap_or_default();
    println!("\n{} {} task(s) to run\n", "→".bold(), group.len());
    for task in &group {
        println!(
            "{} Task {}: {}",
            "▶".bold().cyan(),
            task.id.bold(),
            task.name
        );
    }
    println!();
    run_task_group(&ctx, &group, &spec_content, &workdir).await
}

/// Loop through all parallel groups until all tasks are done or a task fails.
async fn run_all_tasks(ctx: &RunCtx<'_>, spec_content: &str, workdir: &Path) -> Result<()> {
    let mut round = 0usize;

    loop {
        round += 1;
        let (tasks, _) = store::read_tasks(workdir)?;

        let done = tasks
            .iter()
            .filter(|t| t.status == TaskStatus::Done)
            .count();
        let total = tasks.len();

        // Check if everything is done
        if done == total {
            println!("\n{} All {} tasks completed!\n", "✔".bold().green(), total);
            ctx.notifier
                .send(&format!("✅ All {} tasks completed!", total))
                .await
                .ok();
            return Ok(());
        }

        let groups = Resolver::parallel_groups(&tasks);
        if groups.is_empty() {
            println!();
            print_no_eligible_reason(&tasks);
            println!(
                "\n{} Progress: {}/{} tasks done",
                "ℹ".bold().blue(),
                done,
                total
            );
            return Ok(());
        }

        let group = groups.into_iter().next().unwrap_or_default();
        println!(
            "\n{} Round {} — running {} task(s)  ({}/{} done)\n",
            "→".bold(),
            round,
            group.len(),
            done,
            total
        );
        for task in &group {
            println!(
                "{} Task {}: {}",
                "▶".bold().cyan(),
                task.id.bold(),
                task.name
            );
        }
        println!();

        let failed = run_task_group_checked(ctx, &group, spec_content, workdir).await?;

        if failed {
            println!(
                "\n{} Stopping: a task failed. Fix it and re-run.\n",
                "✖".bold().red()
            );
            return Ok(());
        }
    }
}

/// Run a group of tasks sequentially; returns true if any task failed.
/// Tasks in the same group are logically parallel (no shared file deps),
/// but are run one at a time because each needs an exclusive git checkout.
/// True parallelism would require git worktrees — not yet implemented.
async fn run_task_group_checked(
    ctx: &RunCtx<'_>,
    group: &[&Task],
    spec_content: &str,
    workdir: &Path,
) -> Result<bool> {
    let mut any_failed = false;
    for task in group {
        if let Err(e) = run_single_task(ctx, task, spec_content, workdir).await {
            eprintln!("{} Task {} failed: {}", "✖".bold().red(), task.id.bold(), e);
            store::update_task_status(workdir, &task.id, TaskStatus::Failed)?;
            ctx.notifier
                .send(&format!("❌ Task {} failed: {}", task.id, e))
                .await
                .ok();
            git_command(workdir, &["checkout", ctx.default_branch])
                .await
                .ok();
            any_failed = true;
        }
    }
    Ok(any_failed)
}

/// Run a group of tasks (fire and handle errors per task).
async fn run_task_group(
    ctx: &RunCtx<'_>,
    group: &[&Task],
    spec_content: &str,
    workdir: &Path,
) -> Result<()> {
    run_task_group_checked(ctx, group, spec_content, workdir)
        .await
        .map(|_| ())
}

/// Run a single task through the full code → review → loop pipeline.
async fn run_single_task(
    ctx: &RunCtx<'_>,
    task: &Task,
    spec_content: &str,
    workdir: &Path,
) -> Result<()> {
    let config = ctx.config;
    let llm = ctx.llm;
    let review_llm = ctx.review_llm;
    let notifier = ctx.notifier;
    let default_branch = ctx.default_branch;
    let task_detail = store::read_task_detail(workdir, &task.id).unwrap_or_else(|_| {
        format!(
            "Task {}: {}\nScope: {}\nDone when: {}",
            task.id, task.name, task.scope, task.done_when
        )
    });

    // a. Mark as in-progress and commit TASKS.md to the default branch.
    // TASKS.md tracks global state and must be committed before any branch switch,
    // otherwise git refuses to checkout due to local modifications.
    store::update_task_status(workdir, &task.id, TaskStatus::InProgress)?;
    git_command(workdir, &["add", "TASKS.md"]).await.ok();
    git_command(
        workdir,
        &[
            "commit",
            "-m",
            &format!("chore: mark task {} as in-progress", task.id),
        ],
    )
    .await
    .ok(); // ok() — no-op if nothing changed (already committed)

    // b. Create or resume git branch
    let branch = &task.branch;
    let branch_exists = git_command(workdir, &["rev-parse", "--verify", branch])
        .await
        .is_ok();
    if branch_exists {
        println!(
            "{} Branch {} already exists — resuming",
            "⎇".bold(),
            branch.cyan()
        );
        git_command(workdir, &["checkout", branch]).await?;
    } else {
        println!("{} Creating branch: {}", "⎇".bold(), branch.cyan());
        git_command(workdir, &["checkout", "-b", branch]).await?;
    }

    // Sync branch with latest default branch to avoid stale-branch false diffs.
    // Other tasks may have merged to default since this branch was created, making
    // those new files appear as "deleted" in the diff. Merging default in prevents that.
    println!(
        "{} Syncing branch with {}...",
        "⎇".dimmed(),
        default_branch.dimmed()
    );
    match git_command(
        workdir,
        &["merge", "-X", "ours", "--no-edit", default_branch],
    )
    .await
    {
        Ok(_) => {}
        Err(e) => {
            // Non-fatal: sync failed (e.g. unrelated history), continue without it
            git_command(workdir, &["merge", "--abort"]).await.ok();
            println!("{} Branch sync skipped: {}", "⚠".dimmed(), e);
        }
    }

    // c. Notify Telegram
    notifier
        .send(&format!("🚀 Starting task {}: {}", task.id, task.name))
        .await
        .ok();

    let agent = CodingAgent::new(&config.agent.runner_bin);
    let mut loop_ctrl = LoopController::new(config.spec.max_loop_iterations);
    let mut retry_findings: Option<String> = None;

    loop {
        // Check iteration limit
        if let Err(e) = loop_ctrl.increment() {
            eprintln!("{} {} — preserving branch {}", "✖".bold().red(), e, branch);
            git_command(workdir, &["checkout", default_branch])
                .await
                .ok();
            store::update_task_status(workdir, &task.id, TaskStatus::Failed)?;
            git_command(workdir, &["add", "TASKS.md"]).await.ok();
            git_command(
                workdir,
                &[
                    "commit",
                    "-m",
                    &format!("chore: mark task {} as failed", task.id),
                ],
            )
            .await
            .ok();
            notifier
                .send(&format!(
                    "❌ Task {} failed after {} iterations (limit: {})",
                    task.id, e.current, e.max
                ))
                .await
                .ok();
            return Ok(());
        }

        println!(
            "\n{} Iteration {}/{}",
            "↻".bold().yellow(),
            loop_ctrl.iteration(),
            config.spec.max_loop_iterations
        );

        // d. Optionally show agent reasoning before running
        if config.agent.show_reasoning {
            show_reasoning(
                llm,
                task,
                spec_content,
                &task_detail,
                retry_findings.as_deref(),
            )
            .await;
        }

        // e. Spawn coding agent (claude-code or api-agent)
        println!("{} Running coding agent...", "⚙".bold());
        if config.agent.runner == "api-agent" {
            let api_key = crate::config::resolve_review_api_key(config)?;
            let agent_model = if config.llm.model.is_empty() {
                "claude-opus-4-6".to_string()
            } else {
                config.llm.model.clone()
            };
            let api_agent =
                api_agent::ApiCodingAgent::new(api_key, agent_model, config.agent.max_agent_turns);
            let system = API_AGENT_SYSTEM;
            let prompt = match retry_findings.as_deref() {
                Some(findings) => {
                    CodingAgent::build_retry_prompt(task, spec_content, &task_detail, findings)
                }
                None => CodingAgent::build_prompt(task, spec_content, &task_detail),
            };
            api_agent.run(system, &prompt, workdir).await?;
        } else {
            agent
                .run(
                    task,
                    spec_content,
                    &task_detail,
                    workdir,
                    retry_findings.as_deref(),
                )
                .await?;
        }

        // f. Get git diff — capture committed AND uncommitted changes vs default branch.
        // `git diff <branch>` covers working-tree changes; `git diff --cached <branch>`
        // covers staged-but-not-committed. Combined they catch everything Claude wrote,
        // regardless of whether it remembered to `git commit`.
        let committed = git_command(workdir, &["diff", &format!("{}..HEAD", default_branch)])
            .await
            .unwrap_or_default();
        let unstaged = git_command(workdir, &["diff", default_branch])
            .await
            .unwrap_or_default();
        let staged = git_command(workdir, &["diff", "--cached", default_branch])
            .await
            .unwrap_or_default();

        // Collect branch metadata for reviewer context:
        // - commit log (all commits on branch since default_branch)
        // - changed file list (even if diff is truncated)
        let commit_log = git_command(
            workdir,
            &["log", "--oneline", &format!("{}..HEAD", default_branch)],
        )
        .await
        .unwrap_or_default();

        let changed_files = git_command(
            workdir,
            &[
                "diff",
                "--name-status",
                &format!("{}..HEAD", default_branch),
            ],
        )
        .await
        .unwrap_or_default();

        // Prefer committed diff for review; supplement with uncommitted if nothing committed yet
        let diff = if !committed.is_empty() {
            committed
        } else if !unstaged.is_empty() {
            println!(
                "{} Agent has uncommitted changes — running reviews on working-tree diff",
                "⚠".bold().yellow()
            );
            unstaged
        } else if !staged.is_empty() {
            println!(
                "{} Agent has staged but uncommitted changes — reviewing staged diff",
                "⚠".bold().yellow()
            );
            staged
        } else {
            println!(
                "{} No changes detected — agent produced nothing",
                "⚠".bold().yellow()
            );

            if commit_log.trim().is_empty() {
                println!(
                    "{} Branch matches {} — task already completed. Marking done...",
                    "✔".bold().green(),
                    default_branch.bold()
                );
                return finalize_task_done(ctx, workdir, task, branch).await;
            }

            retry_findings = Some(
                "No code was produced. Please implement the task as described. \
                 When done, commit your changes: git add -A && git commit -m \"task impl\""
                    .to_string(),
            );
            continue;
        };

        // g. Run 3 parallel reviews
        // Filter TASKS.md out of the diff — it's specr state, not implementation,
        // and its presence confuses reviewers (they see "done"/"failed" changes, not code).
        let review_diff = filter_diff_paths(&diff, &["TASKS.md"]);
        // changed_files is --name-status format ("M\tTASKS.md"), not unified diff.
        // Use a separate filter that works on that format.
        let review_file_list = filter_name_status(&changed_files, &["TASKS.md"]);

        // If the code diff (excl. TASKS.md) is empty, the implementation was already
        // merged to master via another branch. The diff correctly shows nothing — but
        // reviewers can't assess code they can't see. Mark done immediately.
        if review_diff.trim().is_empty() && review_file_list.trim().is_empty() {
            println!(
                "{} No code changes on branch (implementation already merged to {}) — marking done",
                "✔".bold().green(),
                default_branch.bold()
            );
            return finalize_task_done(ctx, workdir, task, branch).await;
        }

        println!("{} Running reviews...", "🔍".bold());
        let review_timeout = std::time::Duration::from_secs(config.llm.review_timeout_seconds);
        let review_result = tokio::time::timeout(
            review_timeout,
            run_reviews(
                review_llm,
                spec_content,
                &task_detail,
                &review_diff,
                &commit_log,
                &review_file_list,
            ),
        )
        .await
        .map_err(|_| {
            anyhow::anyhow!(
                "Reviews timed out after {}s (adjust llm.review_timeout_seconds)",
                config.llm.review_timeout_seconds
            )
        })??;

        // Print review summary
        print_review_summary(&review_result);

        // h. Check results
        if LoopController::all_passed(&review_result) {
            println!("{} All reviews passed!", "✔".bold().green());
            return finalize_task_done(ctx, workdir, task, branch).await;
        }

        // Has critical findings — print them and loop with feedback
        let findings = LoopController::findings_prompt(&review_result);
        println!("{} Critical findings:\n", "↻".bold().yellow());
        for line in findings.lines() {
            if let Some(rest) = line.strip_prefix("## ") {
                println!("  {}", rest.bold());
            } else if let Some(rest) = line.strip_prefix("### ") {
                println!("  {}", rest.yellow());
            } else if line.starts_with("- ") {
                println!("    {}", line);
            } else if !line.is_empty() {
                println!("  {}", line);
            }
        }
        println!("\n{} Retrying...", "↻".bold().yellow());
        retry_findings = Some(findings);
    }
}

/// Remove diff hunks for specified file paths from a unified diff string.
/// Strips everything from `diff --git a/<path>` to the next `diff --git` header.
async fn finalize_task_done(
    ctx: &RunCtx<'_>,
    workdir: &Path,
    task: &Task,
    branch: &str,
) -> Result<()> {
    let default_branch = ctx.default_branch;

    git_command(workdir, &["checkout", default_branch]).await?;
    if let Err(e) = git_command(workdir, &["merge", "-X", "ours", "--no-edit", branch]).await {
        git_command(workdir, &["merge", "--abort"]).await.ok();
        anyhow::bail!("merge failed (non-TASKS.md conflict): {}", e);
    }
    git_command(workdir, &["branch", "-d", branch]).await.ok();

    store::update_task_status(workdir, &task.id, TaskStatus::Done)?;
    git_command(workdir, &["add", "TASKS.md"]).await.ok();
    git_command(
        workdir,
        &[
            "commit",
            "-m",
            &format!("chore: mark task {} as done", task.id),
        ],
    )
    .await
    .ok();

    ctx.notifier
        .send(&format!("✅ Task {} completed: {}", task.id, task.name))
        .await
        .ok();

    println!(
        "\n{} Task {} merged and marked as done\n",
        "✔".bold().green(),
        task.id.bold()
    );
    Ok(())
}

/// Filter lines from `git diff --name-status` output, excluding paths that
/// contain any of the given strings. Input: "M\tTASKS.md" per line.
fn filter_name_status(name_status: &str, exclude: &[&str]) -> String {
    name_status
        .lines()
        .filter(|line| {
            let path = line.split('\t').nth(1).unwrap_or(line);
            !exclude.iter().any(|ex| path.contains(ex))
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn filter_diff_paths(diff: &str, exclude: &[&str]) -> String {
    let mut result = String::with_capacity(diff.len());
    let mut skip = false;

    for line in diff.lines() {
        if line.starts_with("diff --git ") {
            skip = exclude.iter().any(|p| line.contains(p));
        }
        if !skip {
            result.push_str(line);
            result.push('\n');
        }
    }

    result
}

const API_AGENT_SYSTEM: &str = "\
You are an expert software engineer implementing a specific task in an existing codebase.\n\
\n\
## Tools\n\
- read_file: Read source files before editing. Always read before writing.\n\
- write_file: Write a complete new file or fully replace an existing one.\n\
- edit_file: Replace an exact string in a file. Prefer this for targeted edits — \
do not rewrite entire files just to change a few lines.\n\
- run_command: Run shell commands (cargo, git, grep, find, ls, etc.).\n\
\n\
## Workflow\n\
1. Read SPEC.md and the task file first to understand scope and acceptance criteria.\n\
2. Explore the codebase before writing — read relevant files, check existing patterns.\n\
3. Implement the task. Stay strictly within the scope defined in the task.\n\
4. Run `cargo build` to confirm it compiles.\n\
5. Run `cargo test` — all tests must pass.\n\
6. Run `cargo clippy -- -D warnings` — must be clean.\n\
7. Stage only files you modified: `git add -u && git add <any-new-files-you-created>`\n\
8. Commit: `git commit -m \"task NNN: short description\"`\n\
\n\
## Quality rules\n\
- No `unwrap()` in production code paths — use `?` or explicit error handling.\n\
- No dead code, no unused imports, no clippy warnings.\n\
- Write or update tests when you add logic. Do not delete existing tests.\n\
- Do not touch files listed in \"What NOT to change\" in the task.\n\
- Do not refactor unrelated code — focus only on the task.\n\
\n\
## When you are done\n\
- Confirm all tests pass (`cargo test` output shows 0 failed).\n\
- Confirm clippy is clean.\n\
- Commit all changes with a clear commit message.\n\
- Output a short summary: what you implemented and where.\n\
- Stop — do not start other tasks or make unrequested improvements.\
";

const REASONING_SYSTEM: &str = "\
You are a senior engineer about to implement a task. \
Given the spec and task details, briefly explain your approach in 3-5 bullet points. \
Be concrete: name the files you'll touch, the key design decisions, and any edge cases you'll handle. \
Keep it under 150 words. No preamble, no sign-off — just the bullets.";

/// Ask the LLM for a brief plan before the coding agent runs. Best-effort — never blocks execution.
async fn show_reasoning(
    llm: &dyn LlmClient,
    task: &Task,
    spec_content: &str,
    task_detail: &str,
    retry_findings: Option<&str>,
) {
    let user = if let Some(findings) = retry_findings {
        format!(
            "Task {}: {}\n\nSpec:\n{}\n\nTask detail:\n{}\n\nPrevious review findings to fix:\n{}",
            task.id, task.name, spec_content, task_detail, findings
        )
    } else {
        format!(
            "Task {}: {}\n\nSpec:\n{}\n\nTask detail:\n{}",
            task.id, task.name, spec_content, task_detail
        )
    };

    match llm.complete(REASONING_SYSTEM, &user).await {
        Ok(plan) => {
            println!("\n{}", "💭 Agent plan:".bold().cyan());
            for line in plan.trim().lines() {
                println!("   {}", line);
            }
            println!();
        }
        Err(_) => {
            // Reasoning is best-effort — silently skip on failure
        }
    }
}

fn print_review_summary(result: &review::ReviewResult) {
    let code = if result.code_review.passed() {
        "pass".green()
    } else {
        "FAIL".red()
    };
    let qa = if result.qa_review.passed() {
        "pass".green()
    } else {
        "FAIL".red()
    };
    let style = if result.style_review.passed() {
        "pass".green()
    } else {
        "FAIL".red()
    };

    println!("  Code: {}  QA: {}  Style: {}", code, qa, style);
}

fn print_no_eligible_reason(tasks: &[Task]) {
    println!("{} No eligible tasks to run.\n", "ℹ".bold().blue());

    let open = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Open)
        .count();
    let in_progress = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::InProgress)
        .count();
    let done = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    let failed = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Failed)
        .count();
    let large = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Open && t.size == crate::types::TaskSize::L)
        .count();

    println!(
        "  Open: {}  In-progress: {}  Done: {}  Failed: {}",
        open, in_progress, done, failed
    );

    if large > 0 {
        println!(
            "  {} L-sized task(s) are blocked — split them first with: specr split --task <ID>",
            large
        );
    }

    let blocked: Vec<&Task> = tasks
        .iter()
        .filter(|t| {
            t.status == TaskStatus::Open
                && t.size != crate::types::TaskSize::L
                && !t.depends_on.iter().all(|d| {
                    tasks
                        .iter()
                        .any(|dep| dep.id == *d && dep.status == TaskStatus::Done)
                })
        })
        .collect();

    if !blocked.is_empty() {
        println!("  {} task(s) blocked by unmet dependencies:", blocked.len());
        for t in &blocked {
            println!("    Task {} depends on: {}", t.id, t.depends_on.join(", "));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_filter_diff_paths_removes_tasks_md() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
index abc..def 100644
--- a/src/lib.rs
+++ b/src/lib.rs
@@ -1,2 +1,3 @@
+pub fn hello() {}
diff --git a/TASKS.md b/TASKS.md
index 111..222 100644
--- a/TASKS.md
+++ b/TASKS.md
@@ -1 +1 @@
-status = \"open\"
+status = \"done\"
";
        let filtered = filter_diff_paths(diff, &["TASKS.md"]);
        assert!(filtered.contains("src/lib.rs"));
        assert!(!filtered.contains("TASKS.md"));
        assert!(!filtered.contains("status = \"done\""));
    }

    #[test]
    fn test_filter_diff_paths_keeps_all_when_no_match() {
        let diff = "\
diff --git a/src/main.rs b/src/main.rs
index abc..def 100644
+++ b/src/main.rs
@@ +1 fn main() {}
";
        let filtered = filter_diff_paths(diff, &["TASKS.md"]);
        assert!(filtered.contains("src/main.rs"));
        assert_eq!(filtered.trim(), diff.trim());
    }

    #[test]
    fn test_filter_diff_paths_empty_diff() {
        let filtered = filter_diff_paths("", &["TASKS.md"]);
        assert!(filtered.is_empty());
    }

    #[test]
    fn test_filter_name_status_removes_tasks_md() {
        let input = "M\tTASKS.md\nM\tsrc/lib.rs\nA\tsrc/new.rs";
        let result = filter_name_status(input, &["TASKS.md"]);
        assert!(!result.contains("TASKS.md"));
        assert!(result.contains("src/lib.rs"));
        assert!(result.contains("src/new.rs"));
    }

    #[test]
    fn test_filter_name_status_only_tasks_md_returns_empty() {
        let input = "M\tTASKS.md";
        let result = filter_name_status(input, &["TASKS.md"]);
        assert!(result.trim().is_empty());
    }

    #[test]
    fn test_filter_name_status_empty_input() {
        assert!(filter_name_status("", &["TASKS.md"]).is_empty());
    }

    #[test]
    fn test_filter_diff_paths_multiple_exclusions() {
        let diff = "\
diff --git a/src/lib.rs b/src/lib.rs
+pub fn x() {}
diff --git a/TASKS.md b/TASKS.md
+status = \"done\"
diff --git a/SPEC.md b/SPEC.md
+updated spec
";
        let filtered = filter_diff_paths(diff, &["TASKS.md", "SPEC.md"]);
        assert!(filtered.contains("src/lib.rs"));
        assert!(!filtered.contains("TASKS.md"));
        assert!(!filtered.contains("SPEC.md"));
    }
}
