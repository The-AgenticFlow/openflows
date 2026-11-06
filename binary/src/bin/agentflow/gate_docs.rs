//! `openflows gate docs`: operator control of LORE's post-merge
//! documentation gate.

use anyhow::{Context, Result};
use openflows_harness::Harness;

/// Operator control of LORE's post-merge documentation: decide the exact
/// submitted docs revision and head (run it from the NEXUS workspace: it
/// needs the controller's docs gate secret and Redis credential),
/// pause or resume LORE and the docs PR, restart LORE on a fresh
/// workspace, skip or reopen a ticket's docs, or show the record. A
/// verdict may also be given again after an approval: approve to re-seal
/// it (e.g. after rotating the secret), reject to revoke it.
#[derive(clap::Args)]
#[command(group(clap::ArgGroup::new("docs_action").required(true)))]
pub struct DocsArgs {
    #[arg(long)]
    pub tenant: String,
    /// Ticket to act on (not needed with --pending)
    #[arg(long, required_unless_present = "pending")]
    pub ticket: Option<String>,
    #[arg(long, value_parser=["approve","reject"], group="docs_action", requires_all=["revision","head"])]
    pub verdict: Option<String>,
    #[arg(long, requires = "verdict")]
    pub revision: Option<u64>,
    /// Submitted docs head (full SHA or a unique prefix of 7+ characters)
    #[arg(long, requires = "verdict")]
    pub head: Option<String>,
    /// Decision notes; required when rejecting (sent to LORE as feedback)
    #[arg(long, default_value = "", requires = "verdict")]
    pub notes: String,
    /// Pause LORE for this ticket with a reason. Its workspace is released
    /// (unpushed work is discarded) and the docs PR is held; --resume
    /// starts LORE again on a fresh workspace from the pushed branch
    #[arg(long, value_name = "REASON", group = "docs_action")]
    pub pause: Option<String>,
    /// Resume a paused LORE; it receives its current instruction again
    #[arg(long, group = "docs_action")]
    pub resume: bool,
    /// Re-send LORE its current instruction (stuck or crashed chat)
    #[arg(long, value_name = "REASON", group = "docs_action")]
    pub restart: Option<String>,
    /// Close the ticket's docs without documentation (nothing submitted yet)
    #[arg(long, value_name = "REASON", group = "docs_action")]
    pub skip: Option<String>,
    /// Start a new docs cycle for merged, abandoned or skipped docs; REASON goes to LORE
    #[arg(long, value_name = "REASON", group = "docs_action")]
    pub reopen: Option<String>,
    /// Print the documentation gate record
    #[arg(long, group = "docs_action")]
    pub status: bool,
    /// List docs awaiting a decision or paused, across tickets
    #[arg(long, group = "docs_action")]
    pub pending: bool,
}

pub async fn run(redis_url: &str, args: DocsArgs) -> Result<()> {
    let DocsArgs {
        tenant,
        ticket,
        verdict,
        revision,
        head,
        notes,
        pause,
        resume,
        restart,
        skip,
        reopen,
        status,
        pending,
    } = args;
    use config::docs_lifecycle::DocsEvent;
    let store = Harness::new(redis_url, &tenant).await?;
    if pending {
        return store.docs_pending().await;
    }
    let ticket = ticket.context("--ticket is required")?;
    if status {
        return store.docs_status(&ticket).await;
    }
    let event = if let Some(verdict) = verdict {
        DocsEvent::Decide {
            revision: revision.context("--revision is required with --verdict")?,
            head: head.context("--head is required with --verdict")?,
            approved: verdict == "approve",
            report: notes,
            seal: String::new(),
        }
    } else if let Some(reason) = pause {
        DocsEvent::Pause { reason }
    } else if resume {
        DocsEvent::Resume
    } else if let Some(reason) = restart {
        DocsEvent::Restart { reason }
    } else if let Some(reason) = skip {
        DocsEvent::Skip { reason }
    } else if let Some(reason) = reopen {
        DocsEvent::Reopen { reason }
    } else {
        anyhow::bail!("Choose an action: --verdict, --pause, --resume, --restart, --skip, --reopen, --status or --pending")
    };
    // Only approvals need the keys; an invalid secret fails them loudly.
    let keys = match &event {
        DocsEvent::Decide { approved: true, .. } => {
            config::docs_lifecycle::DocsGateKeys::from_env()?
        }
        _ => None,
    };
    let record = store.docs_operator(&ticket, event, keys.as_ref()).await?;
    println!(
        "Docs gate for {ticket}: {}{}",
        record.phase.as_str(),
        if record.paused { " (paused)" } else { "" }
    );

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Cli, Commands, GateCommands};
    use clap::Parser;

    fn parse(args: &[&str]) -> Result<GateCommands, clap::Error> {
        let mut argv = vec!["openflows", "gate", "docs", "--tenant", "t"];
        argv.extend_from_slice(args);
        Cli::try_parse_from(argv).map(|cli| match cli.command {
            Commands::Gate { action } => action,
            _ => unreachable!(),
        })
    }

    #[test]
    fn exactly_one_docs_action_is_required() {
        assert!(parse(&["--ticket", "T-1"]).is_err());
        assert!(parse(&["--ticket", "T-1", "--resume", "--status"]).is_err());
        assert!(parse(&["--ticket", "T-1", "--pause", "freeze"]).is_ok());
        assert!(parse(&["--pending"]).is_ok(), "--pending needs no ticket");
        assert!(parse(&["--status"]).is_err(), "--status needs a ticket");
    }

    #[test]
    fn verdicts_name_revision_and_head() {
        assert!(parse(&["--ticket", "T-1", "--verdict", "approve"]).is_err());
        assert!(parse(&[
            "--ticket",
            "T-1",
            "--verdict",
            "maybe",
            "--revision",
            "1",
            "--head",
            "abc1234"
        ])
        .is_err());
        assert!(parse(&["--ticket", "T-1", "--notes", "x", "--resume"]).is_err());
        let Ok(GateCommands::Docs(DocsArgs {
            verdict,
            revision,
            head,
            ..
        })) = parse(&[
            "--ticket",
            "T-1",
            "--verdict",
            "reject",
            "--revision",
            "2",
            "--head",
            "abc1234",
            "--notes",
            "Explain the migration",
        ])
        else {
            panic!("valid verdict rejected");
        };
        assert_eq!(verdict.as_deref(), Some("reject"));
        assert_eq!(revision, Some(2));
        assert_eq!(head.as_deref(), Some("abc1234"));
    }
}
