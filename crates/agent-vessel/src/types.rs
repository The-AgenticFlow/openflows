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
    /// Keys that verify operator docs approvals (see
    /// [`config::docs_lifecycle::DocsGateKeys`]). Without them VESSEL never
    /// merges docs PRs.
    pub docs_gate_keys: Option<config::docs_lifecycle::DocsGateKeys>,
}

impl Default for VesselConfig {
    fn default() -> Self {
        Self {
            ci_poll: CiPollConfig::default(),
            merge_method: MergeMethod::default(),
            github_token: String::new(),
            address_review_enabled: true,
            docs_gate_keys: None,
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
            docs_gate_keys: config::docs_lifecycle::gate_keys_from_env_logged(),
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
            docs_gate_keys: config::docs_lifecycle::gate_keys_from_env_logged(),
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
        #[serde(default)]
        head_sha: Option<String>,
    },
    /// No lifecycle ticket owns this PR; a human must handle it.
    Unmanaged { pr_number: u64, reason: String },
    /// A published LORE docs PR cannot merge as it is (conflicts, failed CI,
    /// head moved, or non-documentation files); its docs record goes back to
    /// LORE on the same branch.
    DocsPrRework {
        pr_number: u64,
        reason: String,
        docs_ticket: String,
    },
    /// A LORE docs PR that is not mergeable yet (awaiting the operator,
    /// paused, unverifiable approval, CI still running, or an error reading
    /// its state). It stays pending; nothing else happens.
    DocsPrHeld { pr_number: u64, reason: String },
    /// An operator-approved LORE docs PR merged at its approved head.
    DocsPrMerged {
        pr_number: u64,
        sha: String,
        docs_ticket: String,
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
            VesselOutcome::Unmanaged { .. }
            | VesselOutcome::DocsPrRework { .. }
            | VesselOutcome::DocsPrHeld { .. }
            | VesselOutcome::DocsPrMerged { .. } => None,
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
            VesselOutcome::Unmanaged { pr_number, .. }
            | VesselOutcome::DocsPrRework { pr_number, .. }
            | VesselOutcome::DocsPrHeld { pr_number, .. }
            | VesselOutcome::DocsPrMerged { pr_number, .. } => *pr_number,
        }
    }
}
