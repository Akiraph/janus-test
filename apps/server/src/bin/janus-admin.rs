use anyhow::Context;
use clap::{Parser, Subcommand};
use janus_server::{AppState, config::Config};

#[derive(Parser)]
#[command(name = "janus-admin", about = "Janus server administration")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    IssueInitializationToken,
    IssueRecoveryToken,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    // Parse before touching the data root: `--help` and an unknown subcommand
    // must answer with usage text rather than a configuration or migration
    // failure from a half-opened data root.
    let command = Cli::parse().command;
    let config = Config::from_env().context("invalid Janus configuration")?;
    let data_root = config.data_root.clone();
    let state = AppState::initialize(config)
        .await
        .with_context(|| format!("open the Janus data root at {}", data_root.display()))?;
    match command {
        Command::IssueInitializationToken => {
            let token = state.identity().issue_initialization_token().await?;
            println!("{token}");
        }
        Command::IssueRecoveryToken => {
            println!("{}", state.identity().issue_recovery_token().await?)
        }
    }
    Ok(())
}
