//! openflows-doctor — Coder-only diagnostic CLI.
//!
//! Checks: Coder reachable, pinned version match, >=1 model configured,
//! external auth configured + tenant grants valid, Redis reachable.

use anyhow::Result;
use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(name = "openflows-doctor")]
#[command(about = "Diagnose OpenFlows Coder integration health")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Run all diagnostic checks
    Check,
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let _cmd = cli.command.unwrap_or(Commands::Check);
    openflows::doctor::run_checks().await
}
