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
/// - `run_all = true`: loop through all parallel groups until all tasks done or a failure stops the run.
/// - Default: run the next parallel group, then exit.
pub async fn run(
    config: &Config,
    llm: &dyn LlmClient,
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
        return run_task_group(
            config,
            llm,
            &[task],
            &spec_content,
            &workdir,
            &notifier,
            &default_branch,
        )
        .await;
    }

    // --all: loop until everything is done or a failure stops the run
    if run_all {
        return run_all_tasks(
            config,
            llm,
            &spec_content,
            &workdir,
            &notifier,
            &default_branch,
        )
        .await;
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
    run_task_group(
        config,
        llm,
        &group,
        &spec_content,
        &workdir,
        &notifier,
        &default_branch,
    )
    .await
}

/// Loop through all parallel groups until all tasks are done or a task fails.
async fn run_all_tasks(
    config: &Config,
    llm: &dyn LlmClient,
    spec_content: &str,
    workdir: &Path,
    notifier: &TelegramNotifier,
    default_branch: &str,
) -> Result<()> {
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
            notifier
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

        let failed = run_task_group_checked(
            config,
            llm,
            &group,
            spec_content,
            workdir,
            notifier,
            default_branch,
        )
        .await?;

        if failed {
            println!(
                "\n{} Stopping: a task failed. Fix it and re-run.\n",
                "✖".bold().red()
            );
            return Ok(());
        }
    }
}

/// Run a group of tasks concurrently; returns true if any task failed.
async fn run_task_group_checked(
    config: &Config,
    llm: &dyn LlmClient,
    group: &[&Task],
    spec_content: &str,
    workdir: &Path,
    notifier: &TelegramNotifier,
    default_branch: &str,
) -> Result<bool> {
    let mut any_failed = false;
    for task in group {
        if let Err(e) = run_single_task(
            config,
            llm,
            task,
            spec_content,
            workdir,
            notifier,
            default_branch,
        )
        .await
        {
            eprintln!("{} Task {} failed: {}", "✖".bold().red(), task.id.bold(), e);
            store::update_task_status(workdir, &task.id, TaskStatus::Failed)?;
            notifier
                .send(&format!("❌ Task {} failed: {}", task.id, e))
                .await
                .ok();
            git_command(workdir, &["checkout", default_branch])
                .await
                .ok();
            any_failed = true;
        }
    }
    Ok(any_failed)
}

/// Run a group of tasks (fire and handle errors per task).
async fn run_task_group(
    config: &Config,
    llm: &dyn LlmClient,
    group: &[&Task],
    spec_content: &str,
    workdir: &Path,
    notifier: &TelegramNotifier,
    default_branch: &str,
) -> Result<()> {
    run_task_group_checked(
        config,
        llm,
        group,
        spec_content,
        workdir,
        notifier,
        default_branch,
    )
    .await
    .map(|_| ())
}

/// Run a single task through the full code → review → loop pipeline.
async fn run_single_task(
    config: &Config,
    llm: &dyn LlmClient,
    task: &Task,
    spec_content: &str,
    workdir: &Path,
    notifier: &TelegramNotifier,
    default_branch: &str,
) -> Result<()> {
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

        // d. Spawn coding agent
        println!("{} Running coding agent...", "⚙".bold());
        agent
            .run(
                task,
                spec_content,
                &task_detail,
                workdir,
                retry_findings.as_deref(),
            )
            .await?;

        // e. Get git diff — capture committed AND uncommitted changes vs default branch.
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
            retry_findings = Some(
                "No code was produced. Please implement the task as described. \
                 When done, commit your changes: git add -A && git commit -m \"task impl\""
                    .to_string(),
            );
            continue;
        };

        // f. Run 3 parallel reviews
        // Filter TASKS.md out of the diff — it's specr state, not implementation,
        // and its presence confuses reviewers (they see "done"/"failed" changes, not code).
        let review_diff = filter_diff_paths(&diff, &["TASKS.md"]);

        println!("{} Running reviews...", "🔍".bold());
        let review_timeout = std::time::Duration::from_secs(config.llm.review_timeout_seconds);
        let review_result = tokio::time::timeout(
            review_timeout,
            run_reviews(llm, spec_content, &task_detail, &review_diff),
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

        // g. Check results
        if LoopController::all_passed(&review_result) {
            println!("{} All reviews passed!", "✔".bold().green());

            // Merge branch back to default
            git_command(workdir, &["checkout", default_branch]).await?;
            git_command(workdir, &["merge", branch]).await?;
            git_command(workdir, &["branch", "-d", branch]).await?;

            // Mark done and commit TASKS.md so it's clean for the next checkout
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

            notifier
                .send(&format!("✅ Task {} completed: {}", task.id, task.name))
                .await
                .ok();

            println!(
                "\n{} Task {} merged and marked as done\n",
                "✔".bold().green(),
                task.id.bold()
            );
            return Ok(());
        }

        // Has critical findings — loop with feedback
        let findings = LoopController::findings_prompt(&review_result);
        println!(
            "{} Critical findings detected, retrying...",
            "↻".bold().yellow()
        );
        retry_findings = Some(findings);
    }
}

/// Remove diff hunks for specified file paths from a unified diff string.
/// Strips everything from `diff --git a/<path>` to the next `diff --git` header.
fn filter_diff_paths<'a>(diff: &'a str, exclude: &[&str]) -> String {
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
