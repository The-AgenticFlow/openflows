use anyhow::Result;
use config::Envconfig;
use config::Registry;
use pocketflow_core::{CiPollConfig, MergeMethod};
use serde::{Deserialize, Serialize};
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiReadiness {
    Ready,
    Missing,
    SetupInProgress,
}

#[derive(Debug, Clone)]
pub struct VesselConfig {
    pub ci_poll: CiPollConfig,
    pub merge_method: MergeMethod,
    pub github_token: String,
    /// Whether VESSEL dispatches `/address_review` directives to FORGE's chat
    /// when a PR is in a non-merge-ready review state. When disabled, VESSEL
    /// falls back to the existing file-based rework markers.
    pub address_review_enabled: bool,
}

impl Default for VesselConfig {
    fn default() -> Self {
        Self {
            ci_poll: CiPollConfig::default(),
            merge_method: MergeMethod::default(),
            github_token: String::new(),
            address_review_enabled: true,
        }
    }
}

impl VesselConfig {
    pub fn from_registry(registry_path: impl AsRef<Path>) -> Result<Self> {
        let registry = Registry::load(registry_path)?;
        let github_token = registry.resolve_github_token("vessel")?;

        Ok(Self {
            ci_poll: CiPollConfig::default(),
            merge_method: MergeMethod::default(),
            github_token,
            address_review_enabled: true,
        })
    }

    pub fn from_env() -> Self {
        let github_token = config::GithubConfig::init_from_env()
            .ok()
            .and_then(|g| g.resolve_token())
            .unwrap_or_default();

        Self {
            ci_poll: CiPollConfig::default(),
            merge_method: MergeMethod::default(),
            github_token,
            address_review_enabled: true,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VesselOutcome {
    Merged {
        ticket_id: String,
        pr_number: u64,
        sha: String,
        pr_title: String,
        pr_body: Option<String>,
    },
    CiFailed {
        ticket_id: Option<String>,
        pr_number: u64,
        reason: String,
        failure_detail: Option<github::CiFailureDetail>,
    },
    MergeBlocked {
        ticket_id: Option<String>,
        pr_number: u64,
        reason: String,
    },
    CiTimeout {
        ticket_id: Option<String>,
        pr_number: u64,
    },
    CiMissing {
        ticket_id: Option<String>,
        pr_number: u64,
    },
    Conflicts {
        ticket_id: Option<String>,
        pr_number: u64,
        conflicted_files: Vec<String>,
    },
    /// VESSEL dispatched a `/address_review` directive to FORGE because the PR
    /// is in a non-merge-ready review state (conflicts / changes_requested /
    /// comments). The PR is removed from pending_prs and FORGE re-signals
    /// `review_ready` after rework.
    Reviews {
        ticket_id: Option<String>,
        pr_number: u64,
        state: String,
    },
    DocsPrClosed {
        pr_number: u64,
        reason: String,
    },
}

impl VesselOutcome {
    pub fn ticket_id(&self) -> Option<&str> {
        match self {
            VesselOutcome::Merged { ticket_id, .. } => Some(ticket_id),
            VesselOutcome::CiFailed { ticket_id, .. } => ticket_id.as_deref(),
            VesselOutcome::MergeBlocked { ticket_id, .. } => ticket_id.as_deref(),
            VesselOutcome::CiTimeout { ticket_id, .. } => ticket_id.as_deref(),
            VesselOutcome::CiMissing { ticket_id, .. } => ticket_id.as_deref(),
            VesselOutcome::Conflicts { ticket_id, .. } => ticket_id.as_deref(),
            VesselOutcome::Reviews { ticket_id, .. } => ticket_id.as_deref(),
            VesselOutcome::DocsPrClosed { .. } => None,
        }
    }

    pub fn pr_number(&self) -> u64 {
        match self {
            VesselOutcome::Merged { pr_number, .. } => *pr_number,
            VesselOutcome::CiFailed { pr_number, .. } => *pr_number,
            VesselOutcome::MergeBlocked { pr_number, .. } => *pr_number,
            VesselOutcome::CiTimeout { pr_number, .. } => *pr_number,
            VesselOutcome::CiMissing { pr_number, .. } => *pr_number,
            VesselOutcome::Conflicts { pr_number, .. } => *pr_number,
            VesselOutcome::Reviews { pr_number, .. } => *pr_number,
            VesselOutcome::DocsPrClosed { pr_number, .. } => *pr_number,
        }
    }
}
