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
    let state =
        AppState::initialize(Config::from_env().context("invalid Janus configuration")?).await?;
    match Cli::parse().command {
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
