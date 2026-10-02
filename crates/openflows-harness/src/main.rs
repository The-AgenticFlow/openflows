//! openflows-harness — typed SharedStore CLI for Coder Agent worker workspaces.
//!
//! The Coder Agent invokes this binary via shell (`execute` tool), guided by
//! role skills. It is the ONLY thing that reads/writes Redis from inside a
//! workspace. All writes are validated against typed schemas from `config::state`.
//!
//! Required environment (injected by the workspace template):
//!   REDIS_URL          — Redis SharedStore URL
//!   OPENFLOWS_TENANT   — Tenant identifier (key prefix)
//!   OPENFLOWS_TICKET   — Current ticket ID (e.g., "T-42")
//!   OPENFLOWS_ROLE     — Current role (forge, sentinel, vessel, lore)

use openflows_harness::store;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "openflows-harness")]
#[command(about = "Typed SharedStore CLI for Coder Agent worker workspaces")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Read the task dispatch for this ticket+role
    #[command(name = "dispatch")]
    Dispatch {
        #[command(subcommand)]
        action: DispatchAction,
    },
    /// Set the current phase for this ticket
    #[command(name = "status")]
    Status {
        #[command(subcommand)]
        action: StatusAction,
    },
    /// Write a handoff contract (forge → sentinel)
    #[command(name = "handoff")]
    Handoff {
        #[command(subcommand)]
        action: HandoffAction,
    },
    /// Record that a PR was opened
    #[command(name = "pr")]
    Pr {
        #[command(subcommand)]
        action: PrAction,
    },
    /// Submit a review verdict (sentinel)
    #[command(name = "review")]
    Review {
        #[command(subcommand)]
        action: ReviewAction,
    },
    /// Record that a merge completed (vessel)
    #[command(name = "merge")]
    Merge {
        #[command(subcommand)]
        action: MergeAction,
    },
    /// Manage heartbeat writing (daemonized)
    #[command(name = "heartbeat")]
    Heartbeat {
        #[command(subcommand)]
        action: HeartbeatAction,
    },
    /// Manage phase gates (SENTINEL approval for phase transitions)
    #[command(name = "gate")]
    Gate {
        #[command(subcommand)]
        action: GateAction,
    },
    /// Read/write plan artifacts (FORGE writes, SENTINEL reads)
    #[command(name = "plan")]
    Plan {
        #[command(subcommand)]
        action: PlanAction,
    },
    /// Delegated verification via A2A relay (issue #143)
    #[command(name = "verify")]
    Verify {
        #[command(subcommand)]
        action: VerifyAction,
    },
}

#[derive(Subcommand)]
enum DispatchAction {
    /// Read the dispatch payload for this ticket+role
    Read,
}

#[derive(Subcommand)]
enum StatusAction {
    /// Set the current phase
    Set {
        /// Phase: planning, building, testing, review_ready, blocked
        phase: String,
    },
    /// Read the current status JSON for this ticket (empty JSON if unset)
    Get,
}

#[derive(Subcommand)]
enum HandoffAction {
    /// Write a handoff contract
    Write {
        #[arg(long)]
        contract: PathBuf,
        #[arg(long)]
        notes: Option<String>,
    },
}

#[derive(Subcommand)]
enum PrAction {
    /// Read the recorded PR info for this ticket (empty JSON if unset)
    Get,
    /// Record that a PR was opened
    Opened {
        #[arg(long)]
        pr: u64,
        #[arg(long)]
        branch: String,
        #[arg(long)]
        title: String,
    },
}

#[derive(Subcommand)]
enum ReviewAction {
    /// Submit a review verdict
    Submit {
        #[arg(long)]
        verdict: String,
        #[arg(long)]
        report: PathBuf,
        #[arg(long)]
        pr: Option<u64>,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        round: u64,
        #[arg(long)]
        head: String,
    },
}

#[derive(Subcommand)]
enum MergeAction {
    /// Record that a merge completed
    Done {
        #[arg(long)]
        pr: u64,
        #[arg(long)]
        sha: String,
    },
}

#[derive(Subcommand)]
enum HeartbeatAction {
    /// Start daemonized heartbeat writing (every 30s)
    Start,
    /// Stop heartbeat writing
    Stop,
}

#[derive(Subcommand)]
enum GateAction {
    /// Decide a review of an exact plan revision and (for testing/submit) head.
    Decide {
        #[arg(long)]
        phase: String,
        #[arg(long, value_parser=["approve", "reject"])]
        verdict: String,
        #[arg(long)]
        revision: u64,
        #[arg(long)]
        round: u64,
        #[arg(long)]
        head: Option<String>,
        #[arg(long)]
        report: PathBuf,
    },
    /// Approve a gated phase transition (SENTINEL → FORGE)
    Approve {
        /// Phase to approve (e.g., "planning")
        #[arg(long)]
        phase: String,
        /// Optional notes about the approval
        #[arg(long)]
        notes: Option<String>,
    },
    /// Check gate approval status
    Status {
        /// Phase to check
        #[arg(long)]
        phase: String,
    },
}

#[derive(Subcommand)]
enum PlanAction {
    /// Write a plan to SharedStore (FORGE)
    Write {
        /// Path to the plan file to upload (including Coder chat-specific plan paths)
        #[arg(long)]
        file: PathBuf,
    },
    /// Read a plan from SharedStore (SENTINEL)
    Read,
}

#[derive(Subcommand)]
enum VerifyAction {
    /// Pause testing and ask FORGE to repair infrastructure without changing the candidate
    Repair {
        /// Exact failing command, error and task ID, or missing external prerequisite
        #[arg(long)]
        reason: String,
    },
    /// Prove project readiness in a fresh checkout and publish this shell's environment
    Prepare {
        #[arg(last = true, required = true)]
        command_argv: Vec<String>,
    },
    /// Submit a verify request (SENTINEL-side, task 3 of issue #143)
    Request {
        /// One command token per flag; use --argv=--flag for option tokens
        #[arg(long, conflicts_with = "command_argv")]
        argv: Vec<String>,
        /// Command and arguments after -- (recommended); never interpreted by a shell
        #[arg(last = true, required_unless_present = "argv")]
        command_argv: Vec<String>,
        /// Command execution timeout in seconds
        #[arg(long, default_value = "600")]
        timeout_secs: u64,
        /// Expected exit code (if None, any exit code is acceptable)
        #[arg(long)]
        expect_exit: Option<i32>,
        /// Optional comma-separated list of artifact paths to hash
        #[arg(long)]
        artifacts: Option<String>,
    },
    /// Long-running executor (FORGE-side, task 3 of issue #143)
    Serve,
    /// List recent verification results (humans/audit)
    List {
        /// Filter by pair ID (optional)
        #[arg(long)]
        pair_id: Option<String>,
    },
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("warn")),
        )
        .init();

    let cli = Cli::parse();
    if let Commands::Verify {
        action: VerifyAction::Request {
            argv, command_argv, ..
        },
    } = &cli.command
    {
        a2a_protocol::validate_command_argv(if command_argv.is_empty() {
            argv
        } else {
            command_argv
        })?;
    }

    let env = config::EnvConfig::from_env().context("failed to load environment configuration")?;
    let redis_url = env
        .infra
        .redis_url
        .clone()
        .context("REDIS_URL is not set. This must be injected by the workspace template.")?;
    let tenant =
        env.tenant.tenant.clone().context(
            "OPENFLOWS_TENANT is not set. This must be injected by the workspace template.",
        )?;
    let ticket =
        env.tenant.ticket.clone().context(
            "OPENFLOWS_TICKET is not set. This must be injected by the workspace template.",
        )?;
    let role =
        env.tenant.role.clone().context(
            "OPENFLOWS_ROLE is not set. This must be injected by the workspace template.",
        )?;

    let store = store::HarnessStore::new(&redis_url, &tenant).await?;

    match cli.command {
        Commands::Gate {
            action:
                GateAction::Decide {
                    phase,
                    verdict,
                    revision,
                    round,
                    head,
                    report,
                },
        } => {
            anyhow::ensure!(
                role.eq_ignore_ascii_case("sentinel"),
                "Worker gate decisions require SENTINEL; human decisions use the operator CLI"
            );
            store
                .gate_decide(
                    &ticket,
                    &role,
                    &phase,
                    verdict == "approve",
                    &std::fs::read_to_string(report)?,
                    store::ReviewTarget {
                        revision,
                        round,
                        head,
                    },
                )
                .await?;
        }
        Commands::Dispatch {
            action: DispatchAction::Read,
        } => {
            store.dispatch_read(&ticket, &role).await?;
        }
        Commands::Status {
            action: StatusAction::Set { phase },
        } => {
            store.status_set(&ticket, &role, &phase).await?;
        }
        Commands::Status {
            action: StatusAction::Get,
        } => {
            store.status_get(&ticket).await?;
        }
        Commands::Handoff {
            action: HandoffAction::Write { contract, notes },
        } => {
            store
                .handoff_write(&ticket, &contract, notes.as_deref())
                .await?;
        }
        Commands::Pr {
            action: PrAction::Opened { pr, branch, title },
        } => {
            store.pr_opened(&ticket, &pr, &branch, &title).await?;
        }
        Commands::Pr {
            action: PrAction::Get,
        } => {
            store.pr_get(&ticket).await?;
        }
        Commands::Review {
            action:
                ReviewAction::Submit {
                    verdict,
                    report,
                    pr,
                    revision,
                    round,
                    head,
                },
        } => {
            store
                .review_submit(
                    &ticket,
                    &role,
                    &verdict,
                    &report,
                    pr,
                    store::ReviewTarget {
                        revision,
                        round,
                        head: Some(head),
                    },
                )
                .await?;
        }
        Commands::Merge {
            action: MergeAction::Done { pr, sha },
        } => {
            store.merge_done(&ticket, &pr, &sha).await?;
        }
        Commands::Heartbeat {
            action: HeartbeatAction::Start,
        } => {
            store.heartbeat_start(&ticket, &role).await?;
        }
        Commands::Heartbeat {
            action: HeartbeatAction::Stop,
        } => {
            store.heartbeat_stop(&ticket, &role).await?;
        }
        Commands::Plan {
            action: PlanAction::Write { file },
        } => {
            store.plan_write(&ticket, &file).await?;
        }
        Commands::Plan {
            action: PlanAction::Read,
        } => {
            store.plan_read(&ticket).await?;
        }
        Commands::Gate {
            action: GateAction::Approve { phase, notes },
        } => {
            store
                .gate_approve(&ticket, &role, &phase, notes.as_deref())
                .await?;
        }
        Commands::Gate {
            action: GateAction::Status { phase },
        } => {
            store.gate_status(&ticket, &phase).await?;
        }
        Commands::Verify {
            action: VerifyAction::Repair { reason },
        } => {
            store.verification_repair(&ticket, &role, reason).await?;
        }
        Commands::Verify {
            action: VerifyAction::Prepare { command_argv },
        } => {
            anyhow::ensure!(
                role == "forge",
                "Only FORGE prepares the verification environment"
            );
            let state = store.lifecycle_state(&ticket).await?;
            anyhow::ensure!(
                matches!(
                    state.phase,
                    config::lifecycle::Phase::Building | config::lifecycle::Phase::Blocked
                ),
                "Prepare the environment during building or blocked recovery, not during review"
            );
            openflows_harness::sandbox::PreparedEnvironment::prepare(
                &std::env::current_dir()?,
                &command_argv,
                state.version,
            )?;
            println!("Project environment prepared for this HEAD; readiness is not gate evidence.");
        }
        Commands::Verify {
            action:
                VerifyAction::Request {
                    argv,
                    command_argv,
                    timeout_secs,
                    expect_exit,
                    artifacts,
                },
        } => {
            store
                .verify_request(
                    &ticket,
                    if command_argv.is_empty() {
                        argv
                    } else {
                        command_argv
                    },
                    timeout_secs,
                    expect_exit,
                    artifacts.as_deref(),
                )
                .await?;
        }
        Commands::Verify {
            action: VerifyAction::Serve,
        } => {
            store.verify_serve(&ticket, &role).await?;
        }
        Commands::Verify {
            action: VerifyAction::List { pair_id },
        } => {
            store.verify_list(pair_id.as_deref()).await?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod verify_cli_tests {
    use super::*;

    #[test]
    fn verification_accepts_command_after_separator() {
        let result = Cli::try_parse_from([
            "openflows-harness",
            "verify",
            "request",
            "--expect-exit",
            "0",
            "--",
            "cargo",
            "test",
            "--workspace",
            "--all-features",
            "--",
            "--nocapture",
        ]);
        let cli = result.unwrap_or_else(|error| panic!("{error}"));
        let Commands::Verify {
            action: VerifyAction::Request {
                command_argv, argv, ..
            },
        } = cli.command
        else {
            panic!("wrong command")
        };
        assert!(argv.is_empty());
        assert_eq!(
            command_argv,
            [
                "cargo",
                "test",
                "--workspace",
                "--all-features",
                "--",
                "--nocapture"
            ]
        );
        a2a_protocol::validate_command_argv(&command_argv).unwrap();
    }
    #[test]
    fn legacy_argv_preserves_tokens_and_flags() {
        let cli = Cli::try_parse_from([
            "openflows-harness",
            "verify",
            "request",
            "--expect-exit",
            "0",
            "--argv",
            "cargo",
            "--argv",
            "test",
            "--argv=--workspace",
        ])
        .unwrap();
        let Commands::Verify {
            action: VerifyAction::Request {
                argv, command_argv, ..
            },
        } = cli.command
        else {
            panic!("wrong command")
        };
        assert!(command_argv.is_empty());
        assert_eq!(argv, ["cargo", "test", "--workspace"]);
        a2a_protocol::validate_command_argv(&argv).unwrap();
    }

    #[test]
    fn quoted_command_is_an_actionable_error_not_an_allowlist_blocker() {
        let cli = Cli::try_parse_from([
            "openflows-harness",
            "verify",
            "request",
            "--argv",
            "cargo test",
        ])
        .unwrap();
        let Commands::Verify {
            action: VerifyAction::Request { argv, .. },
        } = cli.command
        else {
            panic!("wrong command")
        };
        let error = a2a_protocol::validate_command_argv(&argv)
            .unwrap_err()
            .to_string();
        assert!(error.contains("single argument"));
        assert!(error.contains("-- cargo test"));
    }

    #[test]
    fn missing_or_ambiguous_command_is_rejected() {
        for args in [
            vec!["openflows-harness", "verify", "request"],
            vec![
                "openflows-harness",
                "verify",
                "request",
                "--argv",
                "cargo",
                "--",
                "npm",
                "test",
            ],
        ] {
            assert!(Cli::try_parse_from(args).is_err());
        }
    }
}
