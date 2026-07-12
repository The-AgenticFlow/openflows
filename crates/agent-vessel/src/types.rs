use anyhow::Result;
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

#[derive(Debug, Clone, Default)]
pub struct VesselConfig {
    pub ci_poll: CiPollConfig,
    pub merge_method: MergeMethod,
    pub github_token: String,
}

impl VesselConfig {
    pub fn from_registry(registry_path: impl AsRef<Path>) -> Result<Self> {
        let registry = Registry::load(registry_path)?;
        let github_token = registry.resolve_github_token("vessel")?;

        Ok(Self {
            ci_poll: CiPollConfig::default(),
            merge_method: MergeMethod::default(),
            github_token,
        })
    }

    pub fn from_env() -> Self {
        let github_token = std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN")
            .or_else(|_| std::env::var("CODER_GITHUB_TOKEN"))
            .unwrap_or_default();

        Self {
            ci_poll: CiPollConfig::default(),
            merge_method: MergeMethod::default(),
            github_token,
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
            VesselOutcome::DocsPrClosed { pr_number, .. } => *pr_number,
        }
    }
}
