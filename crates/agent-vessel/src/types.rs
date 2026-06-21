// crates/agent-vessel/src/types.rs
//
// VESSEL-specific types and configuration.

use anyhow::Result;
use config::Registry;
use pocketflow_core::{CiPollConfig, MergeMethod};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// CI readiness state — mirrors the nexus CiReadiness for store deserialization.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiReadiness {
    Ready,
    Missing,
    SetupInProgress,
}

/// Configuration for the VESSEL agent.
#[derive(Debug, Clone, Default)]
pub struct VesselConfig {
    pub ci_poll: CiPollConfig,
    pub merge_method: MergeMethod,
    pub github_token: String,
}

impl VesselConfig {
    /// Create config using per-agent token from registry (if configured).
    /// Falls back to GITHUB_PERSONAL_ACCESS_TOKEN for backward compatibility.
    pub fn from_registry(registry_path: impl AsRef<Path>) -> Result<Self> {
        let registry = Registry::load(registry_path)?;
        let github_token = registry.resolve_github_token("vessel")?;

        Ok(Self {
            ci_poll: CiPollConfig::default(),
            merge_method: MergeMethod::default(),
            github_token,
        })
    }

    /// Create config using GITHUB_PERSONAL_ACCESS_TOKEN (fallback).
    pub fn from_env() -> Self {
        let github_token = std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN").expect(
            "GITHUB_PERSONAL_ACCESS_TOKEN (or AGENT_VESSEL_GITHUB_TOKEN via registry) must be set",
        );

        Self {
            ci_poll: CiPollConfig::default(),
            merge_method: MergeMethod::default(),
            github_token,
        }
    }
}

/// Result of the VESSEL workflow for a single PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum VesselOutcome {
    /// Successfully merged and optionally deployed
    Merged {
        ticket_id: String,
        pr_number: u64,
        sha: String,
        pr_title: String,
        pr_body: Option<String>,
    },
    /// CI failed, did not merge
    CiFailed {
        ticket_id: Option<String>,
        pr_number: u64,
        reason: String,
        failure_detail: Option<github::CiFailureDetail>,
    },
    /// CI passed but merge failed (conflict, etc.)
    MergeBlocked {
        ticket_id: Option<String>,
        pr_number: u64,
        reason: String,
    },
    /// CI polling timed out
    CiTimeout {
        ticket_id: Option<String>,
        pr_number: u64,
    },
    /// No CI workflows configured — merged without CI validation
    CiMissing {
        ticket_id: Option<String>,
        pr_number: u64,
    },
    /// Merge conflicts detected — could not be auto-resolved
    Conflicts {
        ticket_id: Option<String>,
        pr_number: u64,
        conflicted_files: Vec<String>,
    },
    /// Docs PR with conflicts — closed to allow lore to regenerate
    DocsPrClosed { pr_number: u64, reason: String },
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
