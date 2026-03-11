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

/// Run the agent pipeline for eligible (or specified) tasks.
pub async fn run(config: &Config, llm: &dyn LlmClient, task_id: Option<&str>) -> Result<()> {
    let workdir = std::env::current_dir()?;
    let (tasks, _version) = store::read_tasks(&workdir)?;
    let spec_content = store::read_spec(&workdir)?;

    let notifier = TelegramNotifier::from_config(config);

    let eligible: Vec<&Task> = if let Some(id) = task_id {
        let task = tasks
            .iter()
            .find(|t| t.id == id)
            .with_context(|| format!("Task {} not found", id))?;

        if task.status != TaskStatus::Open {
            anyhow::bail!(
                "Task {} has status '{}', expected 'open'",
                id,
                task.status
            );
        }

        vec![task]
    } else {
        let eligible = Resolver::eligible(&tasks);
        if eligible.is_empty() {
            print_no_eligible_reason(&tasks);
            return Ok(());
        }
        eligible
    };

    println!(
        "\n{} {} task(s) to run\n",
        "→".bold(),
        eligible.len()
    );

    for task in &eligible {
        println!(
            "{} Task {}: {}",
            "▶".bold().cyan(),
            task.id.bold(),
            task.name
        );
    }
    println!();

    // Run tasks from first parallel group (or all if specified via --task)
    let task_group = if task_id.is_some() {
        eligible
    } else {
        let groups = Resolver::parallel_groups(&tasks);
        if groups.is_empty() {
            return Ok(());
        }
        groups.into_iter().next().unwrap_or_default()
    };

    for task in task_group {
        if let Err(e) = run_single_task(config, llm, task, &spec_content, &workdir, &notifier).await
        {
            eprintln!(
                "{} Task {} failed: {}",
                "✖".bold().red(),
                task.id.bold(),
                e
            );
            store::update_task_status(&workdir, &task.id, TaskStatus::Failed)?;
            notifier
                .send(&format!("❌ Task {} failed: {}", task.id, e))
                .await
                .ok();

            // Return to main branch on failure
            git_command(&workdir, &["checkout", "main"]).await.ok();
        }
    }

    Ok(())
}

/// Run a single task through the full code → review → loop pipeline.
async fn run_single_task(
    config: &Config,
    llm: &dyn LlmClient,
    task: &Task,
    spec_content: &str,
    workdir: &Path,
    notifier: &TelegramNotifier,
) -> Result<()> {
    let task_detail = store::read_task_detail(workdir, &task.id).unwrap_or_else(|_| {
        format!(
            "Task {}: {}\nScope: {}\nDone when: {}",
            task.id, task.name, task.scope, task.done_when
        )
    });

    // a. Mark as in-progress
    store::update_task_status(workdir, &task.id, TaskStatus::InProgress)?;

    // b. Create git branch
    let branch = &task.branch;
    println!(
        "{} Creating branch: {}",
        "⎇".bold(),
        branch.cyan()
    );
    git_command(workdir, &["checkout", "-b", branch]).await?;

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
            eprintln!(
                "{} {} — preserving branch {}",
                "✖".bold().red(),
                e,
                branch
            );
            store::update_task_status(workdir, &task.id, TaskStatus::Failed)?;
            notifier
                .send(&format!(
                    "❌ Task {} failed after {} iterations (limit: {})",
                    task.id, e.current, e.max
                ))
                .await
                .ok();
            git_command(workdir, &["checkout", "main"]).await.ok();
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
            .run(task, spec_content, &task_detail, workdir, retry_findings.as_deref())
            .await?;

        // e. Get git diff
        let diff = git_command(workdir, &["diff", "main..HEAD"]).await?;
        if diff.is_empty() {
            println!(
                "{} No changes produced by coding agent",
                "⚠".bold().yellow()
            );
            continue;
        }

        // f. Run 3 parallel reviews
        println!("{} Running reviews...", "🔍".bold());
        let review_result = run_reviews(llm, spec_content, &task_detail, &diff).await?;

        // Print review summary
        print_review_summary(&review_result);

        // g. Check results
        if LoopController::all_passed(&review_result) {
            println!("{} All reviews passed!", "✔".bold().green());

            // Merge branch
            git_command(workdir, &["checkout", "main"]).await?;
            git_command(workdir, &["merge", branch]).await?;
            git_command(workdir, &["branch", "-d", branch]).await?;

            // Mark done
            store::update_task_status(workdir, &task.id, TaskStatus::Done)?;

            notifier
                .send(&format!(
                    "✅ Task {} completed: {}",
                    task.id, task.name
                ))
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

    let open = tasks.iter().filter(|t| t.status == TaskStatus::Open).count();
    let in_progress = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::InProgress)
        .count();
    let done = tasks.iter().filter(|t| t.status == TaskStatus::Done).count();
    let failed = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Failed)
        .count();
    let large = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Open && t.size == crate::types::TaskSize::L)
        .count();

    println!("  Open: {}  In-progress: {}  Done: {}  Failed: {}", open, in_progress, done, failed);

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
