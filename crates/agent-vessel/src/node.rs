// crates/agent-vessel/src/node.rs
//
// VesselNode — orchestrates CI polling, merging, and notification.
// Implements the Node trait for integration with the Flow.

use anyhow::Result;
use async_trait::async_trait;
use config::{
    state::{KEY_PENDING_PRS, KEY_TICKETS, KEY_WORKER_SLOTS},
    Ticket, TicketStatus, WorkerSlot, WorkerStatus, ACTION_CI_FIX_NEEDED,
    ACTION_CONFLICTS_DETECTED,
};
use pocketflow_core::{Action, CiStatus, Node, PrInfo, SharedStore};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use tracing::{debug, info, warn};

use crate::ci_poller::CiPollResult;
use crate::conflict_resolver::ConflictResolver;
use crate::types::{VesselConfig, VesselOutcome};
use crate::{CiPoller, PrMerger, VesselNotifier};

/// VESSEL Node — DevOps Specialist and Merge Gatekeeper.
///
/// Three-phase workflow:
/// 1. prep: Read pending PRs from SharedStore
/// 2. exec: Poll CI, detect conflicts, resolve if possible, merge if green, return outcomes
/// 3. post: Emit events, update tickets, return routing action
pub struct VesselNode {
    config: VesselConfig,
    client: github::GithubRestClient,
    poller: CiPoller,
    merger: PrMerger,
}

/// Environment variable for the workspace root directory.
/// Used to locate worktrees for local conflict resolution.
const ENV_WORKSPACE_ROOT: &str = "AGENTFLOW_WORKSPACE_ROOT";

/// Maximum number of conflict resolution attempts before giving up.
const MAX_CONFLICT_RESOLUTION_ATTEMPTS: u32 = 3;

/// Maximum number of CI fix attempts before giving up.
const MAX_CI_FIX_ATTEMPTS: u32 = 3;

/// Lightweight struct to carry PR identification info for CI_FIX.md writing.
struct CiFixPrInfo {
    pr_number: u64,
    head_branch: String,
    /// Actual ticket_id from PR title (may differ from branch name).
    ticket_id: Option<String>,
}

impl VesselNode {
    pub fn new(config: VesselConfig) -> Self {
        let client = github::GithubRestClient::new(&config.github_token);

        Self {
            poller: CiPoller::new(config.ci_poll.clone(), client.clone()),
            merger: PrMerger::new(client.clone(), config.merge_method),
            client,
            config,
        }
    }

    pub fn from_env() -> Self {
        let registry_path = std::env::current_dir()
            .ok()
            .map(|p| p.join("orchestration").join("agent").join("registry.json"));

        let config = match registry_path {
            Some(path) if path.exists() => {
                info!(registry_path = %path.display(), "VESSEL loading config from registry");
                VesselConfig::from_registry(&path).unwrap_or_else(|e| {
                    warn!(error = %e, "VESSEL failed to load from registry, falling back to GITHUB_PERSONAL_ACCESS_TOKEN");
                    VesselConfig::from_env()
                })
            }
            Some(path) => {
                warn!(path = %path.display(), "VESSEL registry path does not exist, using fallback token");
                VesselConfig::from_env()
            }
            _ => {
                warn!("VESSEL could not determine registry path, using fallback token");
                VesselConfig::from_env()
            }
        };
        info!(
            token_prefix = &config.github_token[..20.min(config.github_token.len())],
            "VESSEL token loaded"
        );
        Self::new(config)
    }

    fn resolve_worktree_path(&self, pr_info: &PrInfo) -> Option<PathBuf> {
        let workspace_root = std::env::var(ENV_WORKSPACE_ROOT).ok()?;
        let branch = &pr_info.head_branch;
        let parts: Vec<&str> = branch.splitn(2, '/').collect();
        if parts.len() != 2 {
            return None;
        }
        let pair_id = parts[0];
        // Worktrees are keyed by pair_id only (not pair_id-ticket_id).
        // See WorktreeManager::create_worktree which uses `worktrees_dir.join(pair_id)`.
        Some(
            PathBuf::from(workspace_root)
                .join("worktrees")
                .join(pair_id),
        )
    }
}

#[async_trait]
impl Node for VesselNode {
    fn name(&self) -> &str {
        "vessel"
    }

    /// Phase 1: Read pending PRs and CI readiness from SharedStore.
    async fn prep(&self, store: &SharedStore) -> Result<Value> {
        debug!("VESSEL prep: reading pending PRs and CI readiness");

        let repository: Option<String> = store.get_typed("repository").await;
        let pending_prs: Option<Vec<Value>> = store.get_typed("pending_prs").await;
        let ci_readiness: Option<crate::types::CiReadiness> = store.get_typed("ci_readiness").await;

        let (owner, repo) = parse_repository(repository.as_deref());

        let has_ci_workflows = match ci_readiness {
            Some(crate::types::CiReadiness::Ready) => true,
            Some(crate::types::CiReadiness::Missing)
            | Some(crate::types::CiReadiness::SetupInProgress) => false,
            None => {
                if !owner.is_empty() && !repo.is_empty() {
                    self.client.has_workflows(owner, repo).await.unwrap_or(true)
                } else {
                    true
                }
            }
        };

        Ok(json!({
            "owner": owner,
            "repo": repo,
            "pending_prs": pending_prs.unwrap_or_default(),
            "has_ci_workflows": has_ci_workflows,
        }))
    }

    /// Phase 2: Process each pending PR (check CI readiness → poll CI → merge → return outcome).
    async fn exec(&self, prep_result: Value) -> Result<Value> {
        let owner = prep_result["owner"].as_str().unwrap_or("");
        let repo = prep_result["repo"].as_str().unwrap_or("");
        let pending_prs = prep_result["pending_prs"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has_ci_workflows = prep_result["has_ci_workflows"].as_bool().unwrap_or(true);

        if pending_prs.is_empty() {
            info!("No pending PRs to process");
            return Ok(json!({ "outcomes": [], "has_work": false }));
        }

        info!(
            count = pending_prs.len(),
            has_ci_workflows, "Processing pending PRs"
        );

        let mut outcomes = Vec::new();

        for pr in pending_prs {
            let pr_number = pr["number"].as_u64().unwrap_or(0);

            if pr_number == 0 {
                warn!(pr = ?pr, "Skipping invalid PR entry");
                continue;
            }

            debug!(pr_number, "Fetching PR details");

            let pr_info = match self.client.get_pull_request(owner, repo, pr_number).await {
                Ok(info) => info,
                Err(e) => {
                    warn!(pr_number, error = %e, "Failed to fetch PR details, skipping");
                    continue;
                }
            };

            // Check if PR has active check runs, even if ci_readiness says Missing.
            // PR #19 (CI setup) adds CI that runs on itself, so we must check.
            // If check_suites returns anything other than Success, checks exist.
            let pr_has_ci = if !has_ci_workflows {
                match self
                    .client
                    .get_check_suites_status(owner, repo, &pr_info.head_sha)
                    .await
                {
                    Ok(CiStatus::Success) => {
                        // Might be truly no checks, or all passed - verify
                        self.has_any_check_runs(owner, repo, &pr_info.head_sha)
                            .await
                            .unwrap_or(false)
                    }
                    Ok(_) => {
                        info!(
                            pr_number,
                            "PR has check runs despite ci_readiness=Missing — processing with CI"
                        );
                        true
                    }
                    Err(_) => false,
                }
            } else {
                has_ci_workflows
            };

            let outcome = if !pr_has_ci {
                warn!(
                    pr_number,
                    "No CI workflows configured — treating as success and alerting NEXUS"
                );
                self.merge_without_ci(owner, repo, pr_info).await?
            } else {
                self.process_single_pr(owner, repo, pr_info).await?
            };
            outcomes.push(outcome);
        }

        Ok(json!({
            "outcomes": outcomes,
            "has_work": !outcomes.is_empty(),
        }))
    }

    /// Phase 3: Emit events, update SharedStore, recycle workers, return routing action.
    async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action> {
        let outcomes: Vec<VesselOutcome> =
            serde_json::from_value(exec_result["outcomes"].clone()).unwrap_or_default();
        let has_work = exec_result["has_work"].as_bool().unwrap_or(false);

        if !has_work {
            debug!("No PRs were processed");
            return Ok(Action::new("no_work"));
        }

        let pending_prs: Vec<Value> = store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();

        let mut any_success = false;
        let mut any_failure = false;
        let mut any_conflicts = false;
        let mut any_ci_fix = false;
        let mut any_awaiting_human = false;
        let mut failed_ticket_ids: Vec<String> = Vec::new();

        for outcome in outcomes {
            match &outcome {
                VesselOutcome::Merged {
                    ticket_id,
                    pr_number,
                    sha,
                    pr_title,
                    pr_body,
                } => {
                    VesselNotifier::emit_ticket_merged(
                        store,
                        ticket_id,
                        *pr_number,
                        sha,
                        pr_title,
                        pr_body.as_deref(),
                    )
                    .await;
                    VesselNotifier::set_ticket_status_merged(store, ticket_id).await;

                    // Parallelize post-merge operations for reduced latency
                    // Run GitHub issue close concurrently with store updates
                    let ticket_id_clone = ticket_id.clone();
                    let pr_number_val = *pr_number;
                    tokio::join!(
                        // GitHub API call - network I/O
                        async {
                            self.close_github_issue(store, &ticket_id_clone).await;
                        },
                        // Store operations - local I/O
                        async {
                            self.update_ticket_status(store, &ticket_id_clone, "merged")
                                .await;
                            self.remove_from_pending_prs(store, pr_number_val).await;
                        }
                    );

                    if let Some(pr) = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number))
                    {
                        self.recycle_worker(store, pr).await;
                    }

                    any_success = true;
                }
                VesselOutcome::CiFailed {
                    ticket_id,
                    pr_number,
                    reason,
                    failure_detail,
                } => {
                    VesselNotifier::emit_ci_failed(store, ticket_id.as_deref(), *pr_number, reason)
                        .await;
                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));

                    let current_ci_attempts = self.get_ci_fix_attempts(store, *pr_number).await;

                    if current_ci_attempts >= MAX_CI_FIX_ATTEMPTS {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            attempts = current_ci_attempts,
                            "Max CI fix attempts exceeded — marking ticket as failed"
                        );
                        if !failed_ticket_ids.contains(&tid) {
                            self.mark_ticket_failed(
                                store,
                                &tid,
                                &format!(
                                    "CI failed for PR #{} after {} fix attempts",
                                    pr_number, current_ci_attempts
                                ),
                            )
                            .await;
                            failed_ticket_ids.push(tid);
                        }
                        self.remove_from_pending_prs(store, *pr_number).await;
                        any_failure = true;
                        continue;
                    }

                    let worker_id = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number))
                        .and_then(|pr| {
                            let wid = pr["worker_id"].as_str().unwrap_or("");
                            if !wid.is_empty() {
                                return Some(wid.to_string());
                            }
                            Self::derive_worker_id_from_branch(
                                pr["head_branch"].as_str().unwrap_or(""),
                            )
                        });

                    let pr_entry = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number));
                    let head_branch = pr_entry
                        .and_then(|p| p["head_branch"].as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| {
                            ticket_id
                                .as_deref()
                                .map(|tid| format!("unknown-pair/{}", tid))
                                .unwrap_or_else(|| format!("unknown/{}", pr_number))
                        });

                    let ci_fix_md_written = self
                        .write_ci_fix_md(
                            &CiFixPrInfo {
                                pr_number: *pr_number,
                                head_branch: head_branch.clone(),
                                ticket_id: ticket_id.clone(),
                            },
                            reason,
                            failure_detail.as_ref(),
                        )
                        .await;

                    if ci_fix_md_written {
                        info!(
                            pr_number,
                            ticket_id = %tid,
                            "Wrote CI_FIX.md — routing to forge_pair for CI fix"
                        );
                    } else {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            "CI_FIX.md NOT written — CI fix may be incomplete"
                        );
                    }

                    let worker_reassigned = if let Some(ref wid) = worker_id {
                        if self.assign_worker_for_ci_fix(store, wid, &tid).await {
                            true
                        } else {
                            info!(
                                derived_worker = %wid,
                                "Derived worker not available for CI fix, finding idle forge worker as fallback"
                            );
                            if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                                self.assign_worker_for_ci_fix(store, &fallback_id, &tid)
                                    .await
                            } else {
                                false
                            }
                        }
                    } else {
                        if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                            self.assign_worker_for_ci_fix(store, &fallback_id, &tid)
                                .await
                        } else {
                            false
                        }
                    };

                    self.remove_from_pending_prs(store, *pr_number).await;

                    if worker_reassigned {
                        self.increment_ci_fix_attempts(store, *pr_number).await;
                        any_ci_fix = true;
                    } else {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            "No worker available for CI fix — marking ticket as failed"
                        );
                        if !failed_ticket_ids.contains(&tid) {
                            self.mark_ticket_failed(
                                store,
                                &tid,
                                &format!(
                                    "CI failed for PR #{} — no worker available for fix",
                                    pr_number
                                ),
                            )
                            .await;
                            failed_ticket_ids.push(tid);
                        }
                        any_failure = true;
                    }
                }
                VesselOutcome::MergeBlocked {
                    ticket_id,
                    pr_number,
                    reason,
                } => {
                    VesselNotifier::emit_merge_blocked(
                        store,
                        ticket_id.as_deref(),
                        *pr_number,
                        reason,
                    )
                    .await;

                    if is_merge_conflict_message(reason) {
                        warn!(
                            pr_number,
                            ticket_id = ?ticket_id,
                            "MergeBlocked reason indicates conflicts — tracking to prevent re-add loop"
                        );
                        self.increment_merge_blocked_attempts(store, *pr_number)
                            .await;
                    }

                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));
                    if !failed_ticket_ids.contains(&tid) {
                        self.mark_ticket_failed(
                            store,
                            &tid,
                            &format!("Merge blocked for PR #{}: {}", pr_number, reason),
                        )
                        .await;
                        failed_ticket_ids.push(tid);
                    }
                    self.remove_from_pending_prs(store, *pr_number).await;
                    any_failure = true;
                }
                VesselOutcome::CiTimeout {
                    ticket_id,
                    pr_number,
                } => {
                    VesselNotifier::emit_ci_timeout(store, ticket_id.as_deref(), *pr_number).await;
                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));

                    let current_ci_attempts = self.get_ci_fix_attempts(store, *pr_number).await;

                    if current_ci_attempts >= MAX_CI_FIX_ATTEMPTS {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            attempts = current_ci_attempts,
                            "Max CI fix attempts exceeded after timeout — marking ticket as failed"
                        );
                        if !failed_ticket_ids.contains(&tid) {
                            self.mark_ticket_failed(
                                store,
                                &tid,
                                &format!(
                                    "CI timed out for PR #{} after {} fix attempts",
                                    pr_number, current_ci_attempts
                                ),
                            )
                            .await;
                            failed_ticket_ids.push(tid);
                        }
                        self.remove_from_pending_prs(store, *pr_number).await;
                        any_failure = true;
                        continue;
                    }

                    let pr_entry = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number));
                    let worker_id = pr_entry.and_then(|p| {
                        let wid = p["worker_id"].as_str().unwrap_or("");
                        if !wid.is_empty() {
                            return Some(wid.to_string());
                        }
                        Self::derive_worker_id_from_branch(p["head_branch"].as_str().unwrap_or(""))
                    });
                    let head_branch = pr_entry
                        .and_then(|p| p["head_branch"].as_str().map(|s| s.to_string()))
                        .unwrap_or_else(|| {
                            ticket_id
                                .as_deref()
                                .map(|tid| format!("unknown-pair/{}", tid))
                                .unwrap_or_else(|| format!("unknown/{}", pr_number))
                        });

                    let ci_fix_md_written = self
                        .write_ci_fix_md(
                            &CiFixPrInfo {
                                pr_number: *pr_number,
                                head_branch: head_branch.clone(),
                                ticket_id: ticket_id.clone(),
                            },
                            "CI timed out — possible stuck or flaky CI run",
                            None,
                        )
                        .await;

                    if ci_fix_md_written {
                        info!(
                            pr_number,
                            ticket_id = %tid,
                            "Wrote CI_FIX.md — routing to forge_pair for CI timeout fix"
                        );
                    }

                    let worker_reassigned = if let Some(ref wid) = worker_id {
                        if self.assign_worker_for_ci_fix(store, wid, &tid).await {
                            true
                        } else {
                            info!(
                                derived_worker = %wid,
                                "Derived worker not available for CI timeout fix, finding idle forge worker as fallback"
                            );
                            if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                                self.assign_worker_for_ci_fix(store, &fallback_id, &tid)
                                    .await
                            } else {
                                false
                            }
                        }
                    } else {
                        if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                            self.assign_worker_for_ci_fix(store, &fallback_id, &tid)
                                .await
                        } else {
                            false
                        }
                    };

                    self.remove_from_pending_prs(store, *pr_number).await;

                    if worker_reassigned {
                        self.increment_ci_fix_attempts(store, *pr_number).await;
                        any_ci_fix = true;
                    } else {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            "No worker available for CI timeout fix — marking ticket as failed"
                        );
                        if !failed_ticket_ids.contains(&tid) {
                            self.mark_ticket_failed(
                                store,
                                &tid,
                                &format!(
                                    "CI timed out for PR #{} — no worker available for fix",
                                    pr_number
                                ),
                            )
                            .await;
                            failed_ticket_ids.push(tid);
                        }
                        any_failure = true;
                    }
                }
                VesselOutcome::CiMissing {
                    ticket_id,
                    pr_number,
                } => {
                    VesselNotifier::emit_ci_missing(store, ticket_id.as_deref(), *pr_number).await;
                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));
                    VesselNotifier::emit_ticket_merged(
                        store,
                        &tid,
                        *pr_number,
                        "",
                        "Merged without CI validation",
                        None,
                    )
                    .await;
                    VesselNotifier::set_ticket_status_merged(store, &tid).await;

                    self.update_ticket_status(store, &tid, "merged_no_ci").await;
                    self.close_github_issue(store, &tid).await;
                    self.remove_from_pending_prs(store, *pr_number).await;

                    if let Some(pr) = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number))
                    {
                        self.recycle_worker(store, pr).await;
                    }

                    any_success = true;
                }
                VesselOutcome::Conflicts {
                    ticket_id,
                    pr_number,
                    conflicted_files,
                } => {
                    let tid = ticket_id
                        .clone()
                        .unwrap_or_else(|| format!("T-{}", pr_number));

                    let current_attempts = self
                        .get_conflict_resolution_attempts(store, *pr_number)
                        .await;

                    if current_attempts >= MAX_CONFLICT_RESOLUTION_ATTEMPTS {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            attempts = current_attempts,
                            "Max conflict resolution attempts exceeded — escalating to human intervention"
                        );
                        VesselNotifier::emit_conflicts_detected(
                            store,
                            ticket_id.as_deref(),
                            *pr_number,
                            conflicted_files,
                        )
                        .await;
                        self.mark_ticket_awaiting_human(
                            store,
                            &tid,
                            &format!(
                                "Merge conflicts on PR #{} not resolved after {} attempts — requires human intervention",
                                pr_number, current_attempts
                            ),
                        )
                        .await;
                        self.remove_from_pending_prs(store, *pr_number).await;
                        any_awaiting_human = true;
                        continue;
                    }

                    VesselNotifier::emit_conflicts_detected(
                        store,
                        ticket_id.as_deref(),
                        *pr_number,
                        conflicted_files,
                    )
                    .await;

                    let derived_worker_id = pending_prs
                        .iter()
                        .find(|p| p["number"].as_u64() == Some(*pr_number))
                        .and_then(|pr| {
                            let wid = pr["worker_id"].as_str().unwrap_or("");
                            if !wid.is_empty() {
                                return Some(wid.to_string());
                            }
                            Self::derive_worker_id_from_branch(
                                pr["head_branch"].as_str().unwrap_or(""),
                            )
                        });

                    let worker_reassigned = if let Some(ref wid) = derived_worker_id {
                        if self
                            .assign_worker_for_conflict_rework(store, wid, &tid)
                            .await
                        {
                            true
                        } else {
                            info!(
                                derived_worker = %wid,
                                "Derived worker not available, finding idle forge worker as fallback"
                            );
                            if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                                self.assign_worker_for_conflict_rework(store, &fallback_id, &tid)
                                    .await
                            } else {
                                false
                            }
                        }
                    } else {
                        if let Some(fallback_id) = self.find_idle_forge_worker(store).await {
                            self.assign_worker_for_conflict_rework(store, &fallback_id, &tid)
                                .await
                        } else {
                            false
                        }
                    };

                    self.remove_from_pending_prs(store, *pr_number).await;

                    if worker_reassigned {
                        self.increment_conflict_resolution_attempts(store, *pr_number)
                            .await;
                        any_conflicts = true;
                    } else {
                        warn!(
                            pr_number,
                            ticket_id = %tid,
                            "No worker available for conflict rework — marking ticket as failed"
                        );
                        self.mark_ticket_failed(
                            store,
                            &tid,
                            &format!(
                                "Merge conflicts on PR #{} — no worker available for rework",
                                pr_number
                            ),
                        )
                        .await;
                        any_failure = true;
                    }
                }
                VesselOutcome::DocsPrClosed { pr_number, reason } => {
                    info!(
                        pr_number,
                        reason,
                        "Docs PR closed due to conflicts — lore will regenerate on next deployment"
                    );
                    self.remove_from_pending_prs(store, *pr_number).await;
                    any_success = true;
                }
            }
        }

        if any_awaiting_human {
            Ok(Action::new(Action::AWAITING_HUMAN))
        } else if any_conflicts {
            Ok(Action::new(ACTION_CONFLICTS_DETECTED))
        } else if any_success {
            Ok(Action::DEPLOYED.into())
        } else if any_ci_fix {
            Ok(Action::new(ACTION_CI_FIX_NEEDED))
        } else if any_failure {
            Ok(Action::DEPLOY_FAILED.into())
        } else {
            Ok(Action::new("no_work"))
        }
    }
}

impl VesselNode {
    /// Check if any check runs exist for a commit by querying check-runs API.
    /// Returns true if total_count > 0.
    async fn has_any_check_runs(&self, owner: &str, repo: &str, sha: &str) -> Result<bool> {
        let url = format!(
            "https://api.github.com/repos/{}/{}/commits/{}/check-runs?per_page=1",
            owner, repo, sha
        );

        let client = reqwest::Client::builder()
            .user_agent("AgentFlow-VESSEL/0.1")
            .build()?;

        let resp = client
            .get(&url)
            .header(
                "Authorization",
                format!("Bearer {}", self.config.github_token),
            )
            .header("Accept", "application/vnd.github+json")
            .header("X-GitHub-Api-Version", "2022-11-28")
            .send()
            .await?;

        if !resp.status().is_success() {
            return Ok(false);
        }

        let body: serde_json::Value = resp.json().await.unwrap_or_default();
        let total = body["total_count"].as_u64().unwrap_or(0);
        Ok(total > 0)
    }

    /// Process a single PR: poll CI → detect conflicts → resolve if possible → merge if green → return outcome.
    /// For Docs PRs: short-circuit CI polling if conflicts detected to save time.
    async fn process_single_pr(
        &self,
        owner: &str,
        repo: &str,
        pr_info: PrInfo,
    ) -> Result<VesselOutcome> {
        let pr_number = pr_info.number;

        let ticket_id = if Self::is_docs_pr(&pr_info) {
            Some("T-DOCS".to_string())
        } else {
            pr_info.ticket_id.clone()
        };

        info!(pr_number, ticket_id = ?ticket_id, "Processing PR");

        // Short-circuit for Docs PRs: check mergeability first to skip CI polling if conflicts exist
        if Self::is_docs_pr(&pr_info) {
            if pr_info.has_conflicts() {
                warn!(
                    pr_number,
                    "Docs PR has conflicts — short-circuiting CI poll and closing"
                );
                return self
                    .close_docs_pr_with_conflicts(owner, repo, &pr_info)
                    .await;
            }
            // Re-fetch PR to get fresh mergeability status if not yet computed
            if pr_info.mergeable.is_none() {
                let fresh_pr = self.client.get_pull_request(owner, repo, pr_number).await?;
                if fresh_pr.has_conflicts() {
                    warn!(
                        pr_number,
                        "Docs PR has conflicts (after re-fetch) — short-circuiting CI poll and closing"
                    );
                    return self
                        .close_docs_pr_with_conflicts(owner, repo, &fresh_pr)
                        .await;
                }
            }
        }

        let poll_result = self
            .poller
            .poll_until_terminal(owner, repo, &pr_info)
            .await?;

        match poll_result {
            CiPollResult::Status(CiStatus::Success) => {
                match self.merger.merge(owner, repo, &pr_info).await {
                    Ok(result) if result.merged => Ok(VesselOutcome::Merged {
                        ticket_id: ticket_id.unwrap_or_else(|| format!("T-{}", pr_number)),
                        pr_number,
                        sha: result.sha.unwrap_or_default(),
                        pr_title: pr_info.title,
                        pr_body: pr_info.body,
                    }),
                    Ok(result) if is_merge_conflict_message(&result.message) => {
                        warn!(
                            pr_number,
                            message = %result.message,
                            "Merge blocked by conflicts — routing to conflict handler"
                        );
                        self.handle_conflicts(owner, repo, pr_info).await
                    }
                    Ok(result) => Ok(VesselOutcome::MergeBlocked {
                        ticket_id,
                        pr_number,
                        reason: result.message,
                    }),
                    Err(e) if is_merge_conflict_message(&e.to_string()) => {
                        warn!(
                            pr_number,
                            error = %e,
                            "Merge API error indicates conflicts — routing to conflict handler"
                        );
                        self.handle_conflicts(owner, repo, pr_info).await
                    }
                    Err(e) => Ok(VesselOutcome::MergeBlocked {
                        ticket_id,
                        pr_number,
                        reason: e.to_string(),
                    }),
                }
            }
            CiPollResult::Status(status) => {
                let detail_result = self
                    .poller
                    .client()
                    .get_failed_checks_detail_structured(owner, repo, &pr_info.head_sha)
                    .await;

                let (reason, failure_detail) = match detail_result {
                    Ok(detail) => {
                        let reason = if detail.failed_checks.is_empty() {
                            format!("CI status: {:?}", status)
                        } else {
                            let check_names: Vec<&str> = detail
                                .failed_checks
                                .iter()
                                .map(|c| c.name.as_str())
                                .collect();
                            format!(
                                "CI status: {:?} — failed checks: {}",
                                status,
                                check_names.join(", ")
                            )
                        };
                        (reason, Some(detail))
                    }
                    Err(e) => {
                        warn!(error = %e, "Failed to get detailed CI failure info — using basic reason");
                        (format!("CI status: {:?}", status), None)
                    }
                };
                Ok(VesselOutcome::CiFailed {
                    ticket_id,
                    pr_number,
                    reason,
                    failure_detail,
                })
            }
            CiPollResult::Conflicts => {
                warn!(
                    pr_number,
                    "Merge conflicts detected during CI poll — attempting resolution"
                );
                self.handle_conflicts(owner, repo, pr_info).await
            }
            CiPollResult::Timeout => {
                warn!(
                    pr_number,
                    "CI timed out — checking for conflicts as likely cause"
                );
                let fresh_pr = self.client.get_pull_request(owner, repo, pr_number).await;
                if let Ok(ref info) = fresh_pr {
                    if info.has_conflicts() {
                        warn!(
                            pr_number,
                            "Conflicts found after timeout — treating as conflict case"
                        );
                        return self.handle_conflicts(owner, repo, fresh_pr.unwrap()).await;
                    }
                }
                match self.merger.merge(owner, repo, &pr_info).await {
                    Ok(result) if result.merged => Ok(VesselOutcome::CiMissing {
                        ticket_id,
                        pr_number,
                    }),
                    Ok(_result) => Ok(VesselOutcome::CiTimeout {
                        ticket_id,
                        pr_number,
                    }),
                    Err(_) => Ok(VesselOutcome::CiTimeout {
                        ticket_id,
                        pr_number,
                    }),
                }
            }
        }
    }

    async fn handle_conflicts(
        &self,
        owner: &str,
        repo: &str,
        pr_info: PrInfo,
    ) -> Result<VesselOutcome> {
        let pr_number = pr_info.number;

        if Self::is_docs_pr(&pr_info) {
            info!(
                pr_number,
                branch = %pr_info.head_branch,
                "Docs PR has merge conflicts — closing to allow lore to regenerate"
            );
            return self
                .close_docs_pr_with_conflicts(owner, repo, &pr_info)
                .await;
        }

        let ticket_id = pr_info.ticket_id.clone();
        let worktree_path = self.resolve_worktree_path(&pr_info);

        let conflicted_files = match &worktree_path {
            Some(wt) if wt.exists() => {
                self.merge_origin_main_in_worktree(wt, &pr_info.head_branch)
                    .await
            }
            Some(wt) => {
                warn!(
                    path = %wt.display(),
                    pr_number,
                    "Worktree path resolved but directory missing — falling back to GitHub API"
                );
                self.fetch_conflicted_files_from_github(owner, repo, &pr_info)
                    .await
            }
            None => {
                warn!(
                    pr_number,
                    "No worktree path — cannot merge origin/main locally"
                );
                self.fetch_conflicted_files_from_github(owner, repo, &pr_info)
                    .await
            }
        };

        if let Some(ref wt) = worktree_path {
            let _ = ConflictResolver::abort_rebase(wt).await;
        }

        let resolution_md_written = self
            .write_conflict_resolution_md(&pr_info, &conflicted_files, None)
            .await;

        if resolution_md_written {
            info!(
                pr_number,
                files = conflicted_files.len(),
                "Wrote CONFLICT_RESOLUTION.md — routing to forge_pair for conflict rework"
            );
        } else {
            warn!(
                pr_number,
                files = conflicted_files.len(),
                "CONFLICT_RESOLUTION.md NOT written (workspace root unavailable) — conflict rework may be incomplete"
            );
        }

        Ok(VesselOutcome::Conflicts {
            ticket_id,
            pr_number,
            conflicted_files,
        })
    }

    async fn merge_origin_main_in_worktree(
        &self,
        worktree_path: &PathBuf,
        branch: &str,
    ) -> Vec<String> {
        let _ = ConflictResolver::abort_rebase(worktree_path).await;

        // Detect the default branch instead of hardcoding "main"
        let default_branch = Self::detect_default_branch(worktree_path);
        let origin_ref = format!("origin/{}", default_branch);

        let fetch = tokio::process::Command::new("git")
            .args(["fetch", "origin", &default_branch])
            .current_dir(worktree_path)
            .output()
            .await;

        match fetch {
            Ok(output) if output.status.success() => {}
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                warn!(branch, %stderr, "git fetch {} failed in worktree", origin_ref);
                return vec!["unknown — fetch failed".to_string()];
            }
            Err(e) => {
                warn!(branch, error = %e, "git fetch {} failed in worktree", origin_ref);
                return vec!["unknown — fetch failed".to_string()];
            }
        }

        let merge = tokio::process::Command::new("git")
            .args(["merge", &origin_ref, "--no-edit"])
            .current_dir(worktree_path)
            .output()
            .await;

        match merge {
            Ok(output) if output.status.success() => {
                info!(branch, "{} merged cleanly — no conflicts", origin_ref);
                vec![]
            }
            Ok(output) => {
                let stderr = String::from_utf8_lossy(&output.stderr);
                if stderr.contains("refusing to merge unrelated histories") {
                    warn!(
                        branch,
                        "Unrelated histories — retrying with --allow-unrelated-histories"
                    );
                    let retry = tokio::process::Command::new("git")
                        .args([
                            "merge",
                            &origin_ref,
                            "--no-edit",
                            "--allow-unrelated-histories",
                        ])
                        .current_dir(worktree_path)
                        .output()
                        .await;

                    return match retry {
                        Ok(o) if o.status.success() => {
                            info!(
                                branch,
                                "{} merged cleanly with --allow-unrelated-histories", origin_ref
                            );
                            vec![]
                        }
                        Ok(_) => {
                            let files = self.list_conflicted_files(worktree_path).await;
                            info!(
                                branch,
                                files = files.len(),
                                "Merge with --allow-unrelated-histories produced conflict markers"
                            );
                            files
                        }
                        Err(e) => {
                            warn!(branch, error = %e, "git merge --allow-unrelated-histories failed");
                            vec!["unknown — merge failed".to_string()]
                        }
                    };
                }
                let files = self.list_conflicted_files(worktree_path).await;
                info!(
                    branch,
                    files = files.len(),
                    "Merge produced conflict markers in worktree"
                );
                files
            }
            Err(e) => {
                warn!(branch, error = %e, "git merge {} failed", origin_ref);
                vec!["unknown — merge failed".to_string()]
            }
        }
    }

    /// Detect the repository's default branch by reading origin/HEAD symref,
    /// falling back to checking remote refs, then defaulting to "main".
    fn detect_default_branch(project_root: &Path) -> String {
        // Method 1: Read origin/HEAD symref (most reliable)
        let output = std::process::Command::new("git")
            .args(["symbolic-ref", "refs/remotes/origin/HEAD"])
            .current_dir(project_root)
            .output();

        if let Ok(o) = output {
            if o.status.success() {
                let refname = String::from_utf8_lossy(&o.stdout).trim().to_string();
                if let Some(branch) = refname.strip_prefix("refs/remotes/origin/") {
                    if !branch.is_empty() {
                        return branch.to_string();
                    }
                }
            }
        }

        // Method 2: Try git rev-parse for each candidate
        for candidate in ["main", "master"] {
            let output = std::process::Command::new("git")
                .args(["rev-parse", "--verify", &format!("origin/{}", candidate)])
                .current_dir(project_root)
                .output();
            if let Ok(o) = output {
                if o.status.success() {
                    return candidate.to_string();
                }
            }
        }

        // Final fallback
        warn!("Could not detect default branch, falling back to 'main'");
        "main".to_string()
    }

    async fn list_conflicted_files(&self, worktree_path: &PathBuf) -> Vec<String> {
        let output = tokio::process::Command::new("git")
            .args(["diff", "--name-only", "--diff-filter=U"])
            .current_dir(worktree_path)
            .output()
            .await;

        match output {
            Ok(o) => {
                let stdout = String::from_utf8_lossy(&o.stdout);
                stdout
                    .lines()
                    .map(|l| l.trim().to_string())
                    .filter(|l| !l.is_empty())
                    .collect()
            }
            Err(_) => vec![],
        }
    }

    async fn fetch_conflicted_files_from_github(
        &self,
        owner: &str,
        repo: &str,
        pr_info: &PrInfo,
    ) -> Vec<String> {
        match self
            .client
            .list_conflicted_files(owner, repo, pr_info.number)
            .await
        {
            Ok(files) => files,
            Err(e) => {
                warn!(pr = pr_info.number, error = %e, "Failed to fetch conflicted files from GitHub");
                vec!["unknown — worktree not available, GitHub API failed".to_string()]
            }
        }
    }

    async fn write_conflict_resolution_md(
        &self,
        pr_info: &PrInfo,
        conflicted_files: &[String],
        fallback_ticket_id: Option<&str>,
    ) -> bool {
        let workspace_root = match std::env::var(ENV_WORKSPACE_ROOT).ok() {
            Some(root) => root,
            None => {
                warn!("AGENTFLOW_WORKSPACE_ROOT not set — cannot write CONFLICT_RESOLUTION.md");
                return false;
            }
        };

        let branch = &pr_info.head_branch;
        let parts: Vec<&str> = branch.splitn(2, '/').collect();
        if parts.len() != 2 {
            warn!(
                branch,
                "Cannot parse branch for pair_id — skipping CONFLICT_RESOLUTION.md"
            );
            return false;
        }
        let pair_id = parts[0];

        let _ticket_id = pr_info.ticket_id.clone().unwrap_or_else(|| {
            if let Some(fb) = fallback_ticket_id {
                info!(
                    branch,
                    fallback_ticket_id = fb,
                    "Using fallback ticket_id for CONFLICT_RESOLUTION.md"
                );
                fb.to_string()
            } else {
                let synthetic = format!("T-{}", pr_info.number);
                info!(
                    branch,
                    synthetic_ticket_id = %synthetic,
                    "Using synthetic ticket_id for CONFLICT_RESOLUTION.md"
                );
                synthetic
            }
        });

        let shared_dir = PathBuf::from(&workspace_root)
            .join("worktrees")
            .join(pair_id)
            .join(".pair-shared");

        if !shared_dir.exists() {
            if let Err(e) = tokio::fs::create_dir_all(&shared_dir).await {
                warn!(
                    path = %shared_dir.display(),
                    error = %e,
                    "Failed to create shared directory for CONFLICT_RESOLUTION.md"
                );
                return false;
            }
            info!(path = %shared_dir.display(), "Created shared directory for CONFLICT_RESOLUTION.md");
        }

        let files_list = if conflicted_files.is_empty() {
            "No specific conflicted files detected — resolve all conflict markers.".to_string()
        } else {
            conflicted_files
                .iter()
                .map(|f| format!("- {}", f))
                .collect::<Vec<_>>()
                .join("\n")
        };

        let content = format!(
             "# Conflict Resolution Required\n\n\
              VESSEL detected merge conflicts between your branch and the default branch.\n\
              `git merge origin/<default>` has been run in your worktree — conflict markers are present.\n\n\
             ## Instructions\n\n\
             1. Open each conflicted file listed below\n\
             2. Resolve all conflict markers (`<<<<<<<`, `=======`, `>>>>>>>`)\n\
             3. Choose the correct integration of both sides — do NOT just pick one\n\
             4. Stage the resolved files: `git add -A`\n\
             5. Commit: `git commit -m \"resolve merge conflicts\"`\n\
             6. Push: `git push`\n\
             7. Write STATUS.json with `\"status\": \"PR_OPENED\"` and your PR number\n\n\
             ## Conflicted Files\n\n{}\n\n\
             ## Important\n\n\
             - Do NOT abort the merge — the conflict markers are there for you to resolve\n\
             - Resolve ALL conflict markers before committing\n\
             - After you push, VESSEL will re-monitor CI automatically",
            files_list,
        );

        let path = shared_dir.join("CONFLICT_RESOLUTION.md");
        if let Err(e) = tokio::fs::write(&path, &content).await {
            warn!(path = %path.display(), error = %e, "Failed to write CONFLICT_RESOLUTION.md");
            false
        } else {
            info!(path = %path.display(), "Wrote CONFLICT_RESOLUTION.md for forge conflict rework");
            true
        }
    }

    /// Merge a PR without CI validation (no CI workflows configured).
    /// Still attempts the merge but emits a ci_missing event to alert NEXUS.
    async fn merge_without_ci(
        &self,
        owner: &str,
        repo: &str,
        pr_info: PrInfo,
    ) -> Result<VesselOutcome> {
        let pr_number = pr_info.number;

        let ticket_id = if Self::is_docs_pr(&pr_info) {
            Some("T-DOCS".to_string())
        } else {
            pr_info.ticket_id.clone()
        };

        info!(pr_number, ticket_id = ?ticket_id, "Merging PR without CI — no workflows configured");

        match self.merger.merge(owner, repo, &pr_info).await {
            Ok(result) if result.merged => Ok(VesselOutcome::CiMissing {
                ticket_id,
                pr_number,
            }),
            Ok(result) if is_merge_conflict_message(&result.message) => {
                warn!(
                    pr_number,
                    message = %result.message,
                    "Merge blocked by conflicts (no CI) — routing to conflict handler"
                );
                self.handle_conflicts(owner, repo, pr_info).await
            }
            Ok(result) => Ok(VesselOutcome::MergeBlocked {
                ticket_id,
                pr_number,
                reason: result.message,
            }),
            Err(e) if is_merge_conflict_message(&e.to_string()) => {
                warn!(
                    pr_number,
                    error = %e,
                    "Merge API error indicates conflicts (no CI) — routing to conflict handler"
                );
                self.handle_conflicts(owner, repo, pr_info).await
            }
            Err(e) => Ok(VesselOutcome::MergeBlocked {
                ticket_id,
                pr_number,
                reason: e.to_string(),
            }),
        }
    }

    /// Update ticket status in SharedStore.
    async fn update_ticket_status(&self, store: &SharedStore, ticket_id: &str, status: &str) {
        let mut tickets: Vec<Value> = store.get_typed("tickets").await.unwrap_or_default();

        for ticket in tickets.iter_mut() {
            if ticket["id"].as_str() == Some(ticket_id) {
                ticket["status"] = json!({ "type": status });
                break;
            }
        }

        store.set(KEY_TICKETS, json!(tickets)).await;
    }

    /// Close the corresponding GitHub issue after a successful merge.
    /// Extracts the issue number from the ticket_id format `T-{issue_number:03}`.
    async fn close_github_issue(&self, store: &SharedStore, ticket_id: &str) {
        let issue_number: u64 = match ticket_id.strip_prefix("T-").and_then(|n| n.parse().ok()) {
            Some(n) => n,
            None => {
                warn!(
                    ticket_id,
                    "Cannot extract GitHub issue number from ticket_id — skipping issue close"
                );
                return;
            }
        };

        let repository: Option<String> = store.get_typed("repository").await;
        let (owner, repo) = parse_repository(repository.as_deref());

        if owner.is_empty() || repo.is_empty() {
            warn!(
                ticket_id,
                "Repository info missing — cannot close GitHub issue"
            );
            return;
        }

        match self.client.close_issue(owner, repo, issue_number).await {
            Ok(()) => info!(ticket_id, issue_number, "GitHub issue closed after merge"),
            Err(e) => {
                warn!(ticket_id, issue_number, error = %e, "Failed to close GitHub issue — merge still succeeded")
            }
        }
    }

    async fn mark_ticket_failed(&self, store: &SharedStore, ticket_id: &str, reason: &str) {
        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();

        for ticket in tickets.iter_mut() {
            if ticket.id == ticket_id {
                let attempts = ticket.attempts + 1;
                ticket.attempts = attempts;
                ticket.status = TicketStatus::Failed {
                    worker_id: String::from("vessel"),
                    reason: reason.to_string(),
                    attempts,
                };
                break;
            }
        }

        store.set(KEY_TICKETS, json!(tickets)).await;
    }

    async fn mark_ticket_awaiting_human(&self, store: &SharedStore, ticket_id: &str, reason: &str) {
        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();

        for ticket in tickets.iter_mut() {
            if ticket.id == ticket_id {
                let attempts = ticket.attempts + 1;
                ticket.attempts = attempts;
                ticket.status = TicketStatus::AwaitingHuman {
                    worker_id: String::from("vessel"),
                    reason: reason.to_string(),
                    attempts,
                };
                break;
            }
        }

        store.set(KEY_TICKETS, json!(tickets)).await;
    }

    /// Remove PR from pending_prs list.
    async fn remove_from_pending_prs(&self, store: &SharedStore, pr_number: u64) {
        let mut pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap_or_default();
        pending.retain(|pr| pr["number"].as_u64() != Some(pr_number));
        store.set("pending_prs", json!(pending)).await;
    }

    /// Increment the conflict_resolution_attempts counter for a PR in pending_prs.
    ///
    /// When the PR is removed from pending_prs during conflict routing and later
    /// re-added by the forge_pair node, the counter must be preserved. We store
    /// it in a separate key to survive the pending_prs removal/re-add cycle.
    async fn increment_conflict_resolution_attempts(&self, store: &SharedStore, pr_number: u64) {
        let key = format!("_conflict_attempts_{}", pr_number);
        let current: u32 = store.get_typed::<u32>(&key).await.unwrap_or(0);
        let next = current + 1;
        info!(
            pr_number,
            attempts = next,
            max = MAX_CONFLICT_RESOLUTION_ATTEMPTS,
            "Incremented conflict resolution attempt counter"
        );
        store.set(&key, json!(next)).await;
    }

    /// Get the current conflict resolution attempt count for a PR.
    async fn get_conflict_resolution_attempts(&self, store: &SharedStore, pr_number: u64) -> u32 {
        let key = format!("_conflict_attempts_{}", pr_number);
        store.get_typed::<u32>(&key).await.unwrap_or(0)
    }

    async fn increment_merge_blocked_attempts(&self, store: &SharedStore, pr_number: u64) {
        let key = format!("_merge_blocked_{}", pr_number);
        let current: u32 = store.get_typed::<u32>(&key).await.unwrap_or(0);
        let next = current + 1;
        info!(
            pr_number,
            attempts = next,
            "Incremented merge blocked attempt counter"
        );
        store.set(&key, json!(next)).await;
    }

    fn is_docs_pr(pr_info: &PrInfo) -> bool {
        pr_info.head_branch.starts_with("lore/") || pr_info.ticket_id.as_deref() == Some("T-DOCS")
    }

    /// Close a docs PR that has conflicts, allowing lore to regenerate.
    async fn close_docs_pr_with_conflicts(
        &self,
        owner: &str,
        repo: &str,
        pr_info: &PrInfo,
    ) -> Result<VesselOutcome> {
        let pr_number = pr_info.number;
        let comment = "This documentation PR has merge conflicts with the main branch. \
                       Closing to allow the lore agent to regenerate the documentation. \
                       Lore will create a fresh docs PR on the next deployment cycle.";

        match self
            .client
            .close_pull_request(owner, repo, pr_number, Some(comment))
            .await
        {
            Ok(()) => {
                info!(pr_number, "Closed conflicting docs PR");
                Ok(VesselOutcome::DocsPrClosed {
                    pr_number,
                    reason: "Merge conflicts on docs PR — closed for regeneration".to_string(),
                })
            }
            Err(e) => {
                warn!(pr_number, error = %e, "Failed to close docs PR with conflicts");
                Ok(VesselOutcome::Conflicts {
                    ticket_id: None,
                    pr_number,
                    conflicted_files: vec!["Docs PR could not be closed".to_string()],
                })
            }
        }
    }

    /// Recycle a worker from Done back to Idle after its PR is merged.
    async fn recycle_worker(&self, store: &SharedStore, pr: &Value) {
        let worker_id = pr["worker_id"].as_str().unwrap_or("");
        if worker_id.is_empty() {
            return;
        }

        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        if let Some(slot) = slots.get_mut(worker_id) {
            match &slot.status {
                WorkerStatus::Done { .. } => {
                    info!(worker_id, "Recycling worker from Done to Idle after merge");
                    slot.status = WorkerStatus::Idle;
                    store.set(KEY_WORKER_SLOTS, json!(slots)).await;
                }
                other => {
                    debug!(worker_id, status = ?other, "Worker not in Done state, skipping recycle");
                }
            }
        }
    }

    fn derive_worker_id_from_branch(head_branch: &str) -> Option<String> {
        let parts: Vec<&str> = head_branch.splitn(2, '/').collect();
        if parts.len() == 2 {
            Some(parts[0].to_string())
        } else {
            None
        }
    }

    async fn find_idle_forge_worker(&self, store: &SharedStore) -> Option<String> {
        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let mut forge_slots: Vec<_> = slots
            .iter()
            .filter(|(id, _)| id.starts_with("forge-"))
            .collect();
        forge_slots.sort_by_key(|(id, _)| id.as_str());

        for (id, slot) in forge_slots {
            if matches!(slot.status, WorkerStatus::Idle | WorkerStatus::Done { .. }) {
                return Some(id.clone());
            }
        }
        None
    }

    async fn assign_worker_for_conflict_rework(
        &self,
        store: &SharedStore,
        worker_id: &str,
        ticket_id: &str,
    ) -> bool {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        if let Some(slot) = slots.get_mut(worker_id) {
            let issue_url = match &slot.status {
                WorkerStatus::Done { ticket_id: tid, .. } => {
                    let tickets: Vec<Ticket> =
                        store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                    tickets
                        .iter()
                        .find(|t| t.id == *tid)
                        .and_then(|t| t.issue_url.clone())
                }
                WorkerStatus::Idle => {
                    let tickets: Vec<Ticket> =
                        store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                    tickets
                        .iter()
                        .find(|t| t.id == ticket_id)
                        .and_then(|t| t.issue_url.clone())
                }
                _ => None,
            };
            info!(
                worker_id,
                ticket_id,
                old_status = ?slot.status,
                "Re-assigning worker for conflict rework (→ Assigned)"
            );
            slot.status = WorkerStatus::Assigned {
                ticket_id: ticket_id.to_string(),
                issue_url,
            };
            store.set(KEY_WORKER_SLOTS, json!(slots)).await;
            true
        } else {
            warn!(
                worker_id,
                ticket_id, "Worker slot not found — cannot assign for conflict rework"
            );
            false
        }
    }

    async fn assign_worker_for_ci_fix(
        &self,
        store: &SharedStore,
        worker_id: &str,
        ticket_id: &str,
    ) -> bool {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        if let Some(slot) = slots.get_mut(worker_id) {
            let issue_url = match &slot.status {
                WorkerStatus::Done { ticket_id: tid, .. } => {
                    let tickets: Vec<Ticket> =
                        store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                    tickets
                        .iter()
                        .find(|t| t.id == *tid)
                        .and_then(|t| t.issue_url.clone())
                }
                WorkerStatus::Idle => {
                    let tickets: Vec<Ticket> =
                        store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                    tickets
                        .iter()
                        .find(|t| t.id == ticket_id)
                        .and_then(|t| t.issue_url.clone())
                }
                _ => None,
            };
            info!(
                worker_id,
                ticket_id,
                old_status = ?slot.status,
                "Re-assigning worker for CI fix (→ Assigned)"
            );
            slot.status = WorkerStatus::Assigned {
                ticket_id: ticket_id.to_string(),
                issue_url,
            };
            store.set(KEY_WORKER_SLOTS, json!(slots)).await;

            let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
            if let Some(ticket) = tickets.iter_mut().find(|t| t.id == ticket_id) {
                if !matches!(ticket.status, TicketStatus::InProgress { .. }) {
                    info!(
                        ticket_id,
                        old_status = ?ticket.status,
                        "Updating ticket status to InProgress for CI fix"
                    );
                    ticket.status = TicketStatus::InProgress {
                        worker_id: worker_id.to_string(),
                    };
                    store.set(KEY_TICKETS, json!(tickets)).await;
                }
            }
            true
        } else {
            warn!(
                worker_id,
                ticket_id, "Worker slot not found — cannot assign for CI fix"
            );
            false
        }
    }

    async fn increment_ci_fix_attempts(&self, store: &SharedStore, pr_number: u64) {
        let key = format!("_ci_fix_attempts_{}", pr_number);
        let current: u32 = store.get_typed::<u32>(&key).await.unwrap_or(0);
        let next = current + 1;
        info!(
            pr_number,
            attempts = next,
            max = MAX_CI_FIX_ATTEMPTS,
            "Incremented CI fix attempt counter"
        );
        store.set(&key, json!(next)).await;
    }

    async fn get_ci_fix_attempts(&self, store: &SharedStore, pr_number: u64) -> u32 {
        let key = format!("_ci_fix_attempts_{}", pr_number);
        store.get_typed::<u32>(&key).await.unwrap_or(0)
    }

    async fn write_ci_fix_md(
        &self,
        pr_placeholder: &CiFixPrInfo,
        reason: &str,
        failure_detail: Option<&github::CiFailureDetail>,
    ) -> bool {
        let workspace_root = match std::env::var(ENV_WORKSPACE_ROOT).ok() {
            Some(root) => root,
            None => {
                warn!("AGENTFLOW_WORKSPACE_ROOT not set — cannot write CI_FIX.md");
                return false;
            }
        };

        // Extract pair_id from branch name (e.g., "forge-1/T-005" -> "forge-1")
        let branch = &pr_placeholder.head_branch;
        let parts: Vec<&str> = branch.splitn(2, '/').collect();
        if parts.len() != 2 {
            warn!(
                branch,
                "Cannot parse branch for pair_id — skipping CI_FIX.md"
            );
            return false;
        }
        let pair_id = parts[0];

        // Use ticket_id from PR info (extracted from title), not from branch name.
        // The branch name may be stale or mismatched with the actual ticket.
        // Fall back to branch-derived ticket_id if not available.
        let _ticket_id = pr_placeholder.ticket_id.as_deref().unwrap_or(parts[1]);

        let shared_dir = PathBuf::from(&workspace_root)
            .join("worktrees")
            .join(pair_id)
            .join(".pair-shared");

        // Ensure the shared directory exists before writing CI_FIX.md.
        // The directory may not exist yet if the pair hasn't been provisioned
        // for this ticket, or if the workspace was cleaned up.
        if !shared_dir.exists() {
            if let Err(e) = tokio::fs::create_dir_all(&shared_dir).await {
                warn!(
                    path = %shared_dir.display(),
                    error = %e,
                    "Failed to create shared directory for CI_FIX.md"
                );
                return false;
            }
            info!(path = %shared_dir.display(), "Created shared directory for CI_FIX.md");
        }

        let job_log_section = match failure_detail {
            Some(d) if !d.job_logs.is_empty() => {
                let logs = d
                    .job_logs
                    .iter()
                    .map(|(name, log)| format!("### Job: {}\n```\n{}\n```", name, log))
                    .collect::<Vec<_>>()
                    .join("\n\n");
                format!(
                    "\n## Job Log Output (for reference)\n\n\
                     {}\n\n\
                     **Do NOT try to fix errors from this log alone.** Read .github/workflows/ and run the actual steps locally.",
                    logs
                )
            }
            _ => String::new(),
        };

        let content = format!(
            "# CI Fix Required\n\n\
             VESSEL detected that CI checks failed for PR #{}.\n\n\
             ## Failed Checks\n\n{}\n\n\
             ## How to Fix\n\n\
             The branch has been updated with the latest origin/main (merged in).\n\
             You now have the latest .github/workflows/ files — read them to find the failing jobs.\n\n\
             **IMPORTANT: Do NOT push without running ALL checks locally first.**\n\n\
             1. Read .github/workflows/ — find the workflow(s) matching the failed check names above.\n\
             2. Match the check name to the job name in the workflow YAML.\n\
             3. Install any missing tools the workflow expects (pip, npm, ruff, etc.).\n\
             4. Install project deps as the workflow does (pip install -r requirements.txt, npm ci, etc.).\n\
             5. Run the failing job's exact `run:` steps locally from the workflow YAML.\n\
             6. Fix ALL errors before pushing — do not fix one and push, CI will just fail on the next.\n\
             7. After ALL checks pass locally: `git add -A && git commit -m \"fix CI failures\" && git push`\n\
             8. Write STATUS.json with `\"status\": \"PR_OPENED\"` and your PR number\n\n\
             If merge conflict markers are present in any files, resolve them BEFORE running CI checks.\n\n\
             ## WORKLOG Updates — CRITICAL\n\n\
             You MUST update WORKLOG.md in the shared directory as you work. The watchdog monitors\n\
             WORKLOG.md — if you don't update it, your pair will be killed after 20 minutes of silence.\n\n\
             ## Rules\n\n\
             - Do NOT change the PR description or title\n\
             - Do NOT push blind fixes — always verify locally first\n\
             - Fix ALL errors before pushing — do not fix one and push, CI will just fail on the next\n\
             - Read .github/workflows/ for the exact CI commands — do not guess\n\
             - After you push, VESSEL will re-monitor CI automatically{}",
            pr_placeholder.pr_number,
            reason,
            job_log_section,
        );

        let path = shared_dir.join("CI_FIX.md");
        if let Err(e) = tokio::fs::write(&path, &content).await {
            warn!(path = %path.display(), error = %e, "Failed to write CI_FIX.md");
            false
        } else {
            info!(path = %path.display(), "Wrote CI_FIX.md for forge CI fix");
            true
        }
    }

    /// Reconcile startup: check for PRs that are already merged on GitHub.
    pub async fn reconcile(&self, store: &SharedStore) -> Result<()> {
        info!("Running VESSEL startup reconciliation");

        let repository: Option<String> = store.get_typed("repository").await;
        let pending_prs: Option<Vec<Value>> = store.get_typed("pending_prs").await;
        let (owner, repo) = parse_repository(repository.as_deref());

        let pending = pending_prs.unwrap_or_default();

        for pr in pending {
            let pr_number = pr["number"].as_u64().unwrap_or(0);
            if pr_number == 0 {
                continue;
            }

            if self.client.is_pr_merged(owner, repo, pr_number).await? {
                warn!(pr_number, "Found already-merged PR during reconciliation");

                let ticket_id = pr["ticket_id"].as_str().map(String::from);
                let pr_info = self.client.get_pull_request(owner, repo, pr_number).await;

                if let Ok(info) = pr_info {
                    let tid = ticket_id
                        .or(info.ticket_id.clone())
                        .unwrap_or_else(|| format!("T-{}", pr_number));
                    VesselNotifier::emit_ticket_merged(
                        store,
                        &tid,
                        pr_number,
                        &info.head_sha,
                        &info.title,
                        None,
                    )
                    .await;
                    VesselNotifier::set_ticket_status_merged(store, &tid).await;
                    self.remove_from_pending_prs(store, pr_number).await;
                }
            }
        }

        Ok(())
    }
}

fn is_merge_conflict_message(msg: &str) -> bool {
    let lower = msg.to_lowercase();
    lower.contains("merge conflict")
        || lower.contains("merge_conflict")
        || (lower.contains("405") && lower.contains("method not allowed"))
}

fn parse_repository(repository: Option<&str>) -> (&str, &str) {
    match repository {
        Some(repo) => {
            let parts: Vec<&str> = repo.split('/').collect();
            if parts.len() == 2 {
                (parts[0], parts[1])
            } else {
                ("", "")
            }
        }
        None => ("", ""),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_repository() {
        assert_eq!(parse_repository(Some("owner/repo")), ("owner", "repo"));
        assert_eq!(parse_repository(Some("single")), ("", ""));
        assert_eq!(parse_repository(None), ("", ""));
    }

    #[tokio::test]
    async fn test_prep_reads_pending_prs() {
        let store = SharedStore::new_in_memory();
        store.set("repository", json!("test-owner/test-repo")).await;
        store
            .set(
                "pending_prs",
                json!([
                    {"number": 1, "ticket_id": "T-1"},
                    {"number": 2, "ticket_id": "T-2"},
                ]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let result = node.prep(&store).await.unwrap();

        assert_eq!(result["owner"], "test-owner");
        assert_eq!(result["repo"], "test-repo");
        assert_eq!(result["pending_prs"].as_array().unwrap().len(), 2);
    }

    #[tokio::test]
    async fn test_prep_empty_pending_prs() {
        let store = SharedStore::new_in_memory();

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let result = node.prep(&store).await.unwrap();

        assert_eq!(result["pending_prs"].as_array().unwrap().len(), 0);
    }

    #[tokio::test]
    async fn test_post_handles_merged_outcome() {
        let store = SharedStore::new_in_memory();
        store
            .set("pending_prs", json!([{"number": 42, "ticket_id": "T-42"}]))
            .await;
        store
            .set(
                "tickets",
                json!([{"id": "T-42", "status": {"type": "in_progress"}}]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [VesselOutcome::Merged {
                ticket_id: "T-42".to_string(),
                pr_number: 42,
                sha: "abc123".to_string(),
                pr_title: "Add feature X".to_string(),
                pr_body: Some("Implementation details".to_string()),
            }],
            "has_work": true,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), Action::DEPLOYED);

        let events = store.get_events_since(0).await;
        assert!(events.iter().any(|e| e.event_type == "ticket_merged"));

        let status = store.get("ticket:T-42:status").await;
        assert_eq!(status, Some(json!("Merged")));

        let pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap_or_default();
        assert!(pending.is_empty());
    }

    #[tokio::test]
    async fn test_post_handles_ci_failed_outcome() {
        let store = SharedStore::new_in_memory();

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [VesselOutcome::CiFailed {
                ticket_id: Some("T-42".to_string()),
                pr_number: 42,
                reason: "Tests failed".to_string(),
                failure_detail: None,
            }],
            "has_work": true,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), Action::DEPLOY_FAILED);

        let events = store.get_events_since(0).await;
        assert!(events.iter().any(|e| e.event_type == "ci_failed"));
    }

    #[tokio::test]
    async fn test_post_handles_merge_blocked_outcome() {
        let store = SharedStore::new_in_memory();

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [VesselOutcome::MergeBlocked {
                ticket_id: Some("T-42".to_string()),
                pr_number: 42,
                reason: "Merge conflict".to_string(),
            }],
            "has_work": true,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), Action::DEPLOY_FAILED);

        let events = store.get_events_since(0).await;
        assert!(events.iter().any(|e| e.event_type == "merge_blocked"));
    }

    #[tokio::test]
    async fn test_post_handles_no_work() {
        let store = SharedStore::new_in_memory();

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        let exec_result = json!({
            "outcomes": [],
            "has_work": false,
        });

        let action = node.post(&store, exec_result).await.unwrap();
        assert_eq!(action.as_str(), "no_work");
    }

    #[tokio::test]
    async fn test_update_ticket_status() {
        let store = SharedStore::new_in_memory();
        store
            .set(
                "tickets",
                json!([
                    {"id": "T-1", "status": {"type": "open"}},
                    {"id": "T-42", "status": {"type": "in_progress"}},
                ]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        node.update_ticket_status(&store, "T-42", "merged").await;

        let tickets: Vec<Value> = store.get_typed("tickets").await.unwrap();
        let ticket = tickets.iter().find(|t| t["id"] == "T-42").unwrap();
        assert_eq!(ticket["status"]["type"], "merged");
    }

    #[tokio::test]
    async fn test_remove_from_pending_prs() {
        let store = SharedStore::new_in_memory();
        store
            .set(
                "pending_prs",
                json!([
                    {"number": 1, "ticket_id": "T-1"},
                    {"number": 42, "ticket_id": "T-42"},
                    {"number": 100, "ticket_id": "T-100"},
                ]),
            )
            .await;

        let config = VesselConfig::default();
        let node = VesselNode::new(config);

        node.remove_from_pending_prs(&store, 42).await;

        let pending: Vec<Value> = store.get_typed("pending_prs").await.unwrap();
        assert_eq!(pending.len(), 2);
        assert!(pending.iter().all(|pr| pr["number"] != 42));
    }
}
