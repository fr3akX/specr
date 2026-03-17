mod agent_runner;
mod config;
mod llm;
mod spec_composer;
mod store;
mod task_generator;
mod telegram;
mod tui;
mod types;

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "specr", about = "Turn a rough idea into a structured SPEC.md")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Start a guided Q&A session to compose a new SPEC.md
    Compose {
        /// The initial project idea
        idea: String,
    },
    /// Refine an existing SPEC.md by editing sections
    Refine,
    /// Decompose SPEC.md into an ordered task list
    Tasks,
    /// Open interactive TUI task board
    Status,
    /// Split a large (L) task into subtasks
    Split {
        /// Task ID to split (e.g., 003)
        #[arg(long)]
        task: String,
    },
    /// Run next eligible task(s) through the agent pipeline
    Run {
        /// Run a specific task by ID (e.g., 002)
        #[arg(long)]
        task: Option<String>,
        /// Run continuously until all tasks are done (or a failure occurs)
        #[arg(long)]
        all: bool,
    },
    /// Start Telegram bot polling loop
    Bot,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    if let Commands::Status = cli.command {
        return tui::run();
    }

    let config = config::load_config()?;

    match cli.command {
        Commands::Compose { idea } => {
            let api_key = config::resolve_api_key_optional(&config)?;
            let client = llm::create_client(&config, &api_key)?;
            let output_dir = std::path::Path::new(&config.output.base_dir);
            spec_composer::compose(&idea, &config, client.as_ref(), output_dir).await?;
        }
        Commands::Refine => {
            let api_key = config::resolve_api_key_optional(&config)?;
            let client = llm::create_client(&config, &api_key)?;
            let dir = std::env::current_dir()?;
            spec_composer::refine(&config, client.as_ref(), &dir).await?;
        }
        Commands::Tasks => {
            let api_key = config::resolve_api_key_optional(&config)?;
            let client = llm::create_client(&config, &api_key)?;
            task_generator::run(&config, client.as_ref()).await?;
        }
        Commands::Split { task } => {
            let api_key = config::resolve_api_key_optional(&config)?;
            let client = llm::create_client(&config, &api_key)?;
            task_generator::split(&config, client.as_ref(), &task).await?;
        }
        Commands::Run { task, all } => {
            let api_key = config::resolve_api_key_optional(&config)?;
            let client = llm::create_client(&config, &api_key)?;
            let review_api_key = config::resolve_review_api_key(&config)?;
            let review_client = llm::create_review_client(&config, &review_api_key)?;
            agent_runner::run(
                &config,
                client.as_ref(),
                review_client.as_ref(),
                task.as_deref(),
                all,
            )
            .await?;
        }
        Commands::Bot => {
            telegram::run_bot(&config).await?;
        }
        Commands::Status => unreachable!(),
    }

    Ok(())
}
