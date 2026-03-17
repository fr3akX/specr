pub mod notify;

pub use notify::TelegramNotifier;

use anyhow::{Context, Result};

use crate::config::Config;

/// Run the Telegram bot polling loop.
pub async fn run_bot(config: &Config) -> Result<()> {
    if !config.telegram.enabled {
        anyhow::bail!("Telegram is not enabled in config. Set telegram.enabled = true");
    }

    let bot_token = std::env::var(&config.telegram.bot_token_env)
        .with_context(|| format!("Set {} environment variable", config.telegram.bot_token_env))?;
    let chat_id = std::env::var(&config.telegram.chat_id_env)
        .with_context(|| format!("Set {} environment variable", config.telegram.chat_id_env))?;

    let notifier = TelegramNotifier::new(bot_token.clone(), chat_id);

    println!("Starting Telegram bot (polling)...");
    notifier.send("🤖 specr bot started").await.ok();

    let client = reqwest::Client::new();
    let mut offset: i64 = 0;

    loop {
        let url = format!(
            "https://api.telegram.org/bot{}/getUpdates?offset={}&timeout=30",
            bot_token, offset
        );

        let resp = client.get(&url).send().await;
        let resp = match resp {
            Ok(r) => r,
            Err(e) => {
                eprintln!("Polling error: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        let body: serde_json::Value = match resp.json().await {
            Ok(v) => v,
            Err(e) => {
                eprintln!("JSON parse error: {}", e);
                tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                continue;
            }
        };

        if let Some(updates) = body["result"].as_array() {
            for update in updates {
                if let Some(update_id) = update["update_id"].as_i64() {
                    offset = update_id + 1;
                }

                if let Some(text) = update["message"]["text"].as_str() {
                    let reply = handle_command(text, config).await;
                    notifier.send(&reply).await.ok();
                }
            }
        }
    }
}

/// Handle a bot command and return the reply text.
async fn handle_command(text: &str, config: &Config) -> String {
    let text = text.trim();

    match text {
        "/status" | "status" => {
            let workdir = match std::env::current_dir() {
                Ok(d) => d,
                Err(e) => return format!("Error: {}", e),
            };
            match crate::store::read_tasks(&workdir) {
                Ok((tasks, _version)) => format_task_status(&tasks),
                Err(e) => format!("Error reading tasks: {}", e),
            }
        }
        _ if text.starts_with("/run ") || text.starts_with("run ") => {
            let id = text
                .strip_prefix("/run ")
                .or_else(|| text.strip_prefix("run "))
                .unwrap_or("")
                .trim();
            if id.is_empty() {
                "Usage: run <task_id>".to_string()
            } else {
                match run_task_from_bot(config, id).await {
                    Ok(msg) => msg,
                    Err(e) => format!("Error running task {}: {}", id, e),
                }
            }
        }
        "/run" | "run" => match run_task_from_bot_auto(config).await {
            Ok(msg) => msg,
            Err(e) => format!("Error: {}", e),
        },
        _ => format!("Unknown command: {}. Use: status, run, run <id>", text),
    }
}

fn format_task_status(tasks: &[crate::types::Task]) -> String {
    use crate::types::TaskStatus;

    let mut out = String::from("📋 Task Board\n\n");

    for task in tasks {
        let icon = match task.status {
            TaskStatus::Open => "○",
            TaskStatus::InProgress => "●",
            TaskStatus::Done => "✔",
            TaskStatus::Failed => "✖",
        };
        out.push_str(&format!(
            "{} {} · {} [{}] ({})\n",
            icon, task.id, task.name, task.size, task.status
        ));
    }

    let done = tasks
        .iter()
        .filter(|t| t.status == TaskStatus::Done)
        .count();
    out.push_str(&format!("\nProgress: {}/{}", done, tasks.len()));

    out
}

async fn run_task_from_bot(config: &Config, task_id: &str) -> Result<String> {
    let api_key = crate::config::resolve_api_key_optional(config)?;
    let client = crate::llm::create_client(config, &api_key)?;
    let review_api_key = crate::config::resolve_review_api_key(config)?;
    let review_client = crate::llm::create_review_client(config, &review_api_key)?;
    crate::agent_runner::run(
        config,
        client.as_ref(),
        review_client.as_ref(),
        Some(task_id),
        false,
    )
    .await?;
    Ok(format!("Task {} execution complete", task_id))
}

async fn run_task_from_bot_auto(config: &Config) -> Result<String> {
    let api_key = crate::config::resolve_api_key_optional(config)?;
    let client = crate::llm::create_client(config, &api_key)?;
    let review_api_key = crate::config::resolve_review_api_key(config)?;
    let review_client = crate::llm::create_review_client(config, &review_api_key)?;
    crate::agent_runner::run(config, client.as_ref(), review_client.as_ref(), None, false).await?;
    Ok("Auto-run execution complete".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Task, TaskSize, TaskStatus};

    fn make_task(id: &str, name: &str, status: TaskStatus) -> Task {
        Task {
            id: id.to_string(),
            name: name.to_string(),
            size: TaskSize::S,
            status,
            depends_on: vec![],
            done_when: "tests pass".to_string(),
            scope: "scope".to_string(),
            files_to_touch: vec![],
            not_to_change: vec![],
            branch: format!("task/{}", id),
            interface: None,
        }
    }

    #[test]
    fn test_format_task_status() {
        let tasks = vec![
            make_task("001", "Scaffold", TaskStatus::Done),
            make_task("002", "Models", TaskStatus::InProgress),
            make_task("003", "API", TaskStatus::Open),
            make_task("004", "Broken", TaskStatus::Failed),
        ];
        let output = format_task_status(&tasks);
        assert!(output.contains("✔ 001"));
        assert!(output.contains("● 002"));
        assert!(output.contains("○ 003"));
        assert!(output.contains("✖ 004"));
        assert!(output.contains("Progress: 1/4"));
    }

    #[test]
    fn test_format_task_status_empty() {
        let tasks: Vec<Task> = vec![];
        let output = format_task_status(&tasks);
        assert!(output.contains("Progress: 0/0"));
    }
}
