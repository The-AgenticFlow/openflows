// crates/agent-nexus/src/lib.rs
use agent_client::{AgentDecision, AgentPersona, AgentRunner};
use anyhow::{Context, Result};
use async_trait::async_trait;
use coder_client::{
    AgentStatus, ChatStatus, CoderClient, CreateWorkspaceRequest, WorkspaceStatus, CHAT_LABEL_FLOW,
    CHAT_LABEL_ROLE, CHAT_LABEL_TICKET,
};
use config::{
    state::{
        full_ticket_key, full_ticket_key_flat, heartbeat_key, HeartbeatRecord, KEY_COMMAND_GATE,
        KEY_PENDING_PRS, KEY_TICKETS, KEY_TICKET_CHAT, KEY_TICKET_CHAT_ACTION, KEY_TICKET_DISPATCH,
        KEY_TICKET_RECOVERY_ATTEMPTS, KEY_TICKET_STATUS, KEY_TICKET_WORKSPACE, KEY_WORKER_SLOTS,
    },
    Registry, Ticket, TicketStatus, WorkerSlot, WorkerStatus, WorkspaceProvider, ACTION_MERGE_PRS,
    ACTION_NO_WORK,
};
use openflows_notifier::{NotificationMessage, NotificationService};
use pocketflow_core::{node::STOP_SIGNAL, Action, Node, SharedStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info, warn};

const NO_WORK_THRESHOLD: u32 = 3;
const KEY_NO_WORK_COUNT: &str = "_no_work_count";
const KEY_CI_READINESS: &str = "ci_readiness";
const MAX_CONFLICT_RESOLUTION_ATTEMPTS: u32 = 3;
const HEARTBEAT_STALE_AFTER_SECS: u64 = 90;
/// Maximum CI fix attempts before refusing to re-add a PR.
/// Must match vessel::node::MAX_CI_FIX_ATTEMPTS to stay in sync.
const MAX_CI_FIX_ATTEMPTS_NEXUS: u32 = 3;
const CI_SETUP_TICKET_ID: &str = "T-CI-001";
const ASSIGNMENT_FAILURE_MARKER: &str = "<!-- openflows-assignment-failure -->";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CiReadiness {
    Ready,
    Missing,
    SetupInProgress,
}

fn is_ci_setup_ticket(ticket: &Ticket) -> bool {
    let t = ticket.title.to_lowercase();
    t.contains("ci") && (t.contains("setup") || t.contains("pipeline") || t.contains("workflow"))
        || ticket.id == CI_SETUP_TICKET_ID
        || ticket.id.starts_with("T-CI-")
}

fn has_ci_setup_ticket(tickets: &[Ticket]) -> bool {
    tickets.iter().any(is_ci_setup_ticket)
}

fn ci_setup_ticket_active(tickets: &[Ticket]) -> bool {
    tickets
        .iter()
        .any(|t| is_ci_setup_ticket(t) && t.is_assignable())
}

/// Attempt to normalize an unrecognized STATUS.json status to a known canonical status.
/// This mirrors the keyword-based fuzzy matching in the pair harness so Nexus can
/// re-map blocked tickets without requiring the pair to re-run.
fn remap_unrecognized_status(raw: &str) -> Option<&'static str> {
    let upper = raw.trim().to_uppercase();

    // Same priority ordering as pair_harness::normalize_status keyword matching.
    // More-specific matches checked before less-specific ones.

    // PR-related keywords
    if (upper.contains("PR") || upper.contains("PULL_REQUEST"))
        && (upper.contains("OPEN") || upper.contains("CREAT") || upper.contains("SUBMIT"))
    {
        return Some("PR_OPENED");
    }
    if upper.contains("EXHAUST") || upper.contains("FUEL") || upper.contains("BUDGET") {
        return Some("FUEL_EXHAUSTED");
    }
    // Sentinel checked before generic REVIEW (more specific)
    if upper.contains("SENTINEL") {
        return Some("AWAITING_SENTINEL_REVIEW");
    }
    if upper.contains("APPROVE") || (upper.contains("READY") && !upper.contains("PR")) {
        return Some("APPROVED_READY");
    }
    // Review keywords — exclude if completion keywords also present
    let has_completion_keyword = upper.contains("DONE")
        || upper.contains("COMPLETE")
        || upper.contains("FINISH")
        || upper.contains("SUCCESS");
    if !has_completion_keyword
        && (upper.contains("REVIEW")
            || upper.contains("WAIT")
            || upper.contains("PAUSE")
            || upper.contains("HOLD"))
    {
        return Some("PENDING_REVIEW");
    }
    if upper.contains("DONE")
        || upper.contains("COMPLETE")
        || upper.contains("FINISH")
        || upper.contains("SUCCESS")
    {
        return Some("COMPLETE");
    }
    if upper.contains("BLOCK")
        || upper.contains("FAIL")
        || upper.contains("ERROR")
        || upper.contains("STUCK")
        || upper.contains("ABORT")
        || upper.contains("ABANDON")
        || upper.contains("CANNOT")
    {
        return Some("BLOCKED");
    }
    if upper.contains("SEGMENT") {
        return Some("SEGMENT_N_DONE");
    }
    None
}

/// Auto-resolve tickets that failed due to unrecognized STATUS.json statuses.
/// When FORGE writes an unrecognized status, the pair harness treats it as Blocked.
/// Nexus can re-map the raw status to a known canonical status and reset the ticket
/// so the worker can be re-assigned without the cycle stalling.
fn auto_resolve_unrecognized_statuses(tickets: &mut [Ticket]) -> usize {
    let mut resolved = 0;
    for ticket in tickets.iter_mut() {
        if let TicketStatus::Failed {
            reason,
            worker_id: _,
            attempts: _,
        } = &ticket.status
        {
            if reason.starts_with("Unrecognized STATUS.json status:") {
                // Parse the raw status from the reason string:
                // "Unrecognized STATUS.json status: AWAITING_REVIEW (normalized: AWAITING_REVIEW)"
                let raw_status = reason
                    .strip_prefix("Unrecognized STATUS.json status: ")
                    .and_then(|s| s.split(" (normalized:").next())
                    .unwrap_or("")
                    .trim();

                if let Some(remapped) = remap_unrecognized_status(raw_status) {
                    info!(
                        ticket_id = %ticket.id,
                        raw_status = raw_status,
                        remapped = remapped,
                        "Auto-resolving unrecognized STATUS.json status"
                    );
                    // Non-terminal statuses (PENDING_REVIEW, AWAITING_SENTINEL_REVIEW,
                    // APPROVED_READY, SEGMENT_N_DONE) mean the agent was trying to signal
                    // it needed more work/review — reset ticket so it can be re-assigned.
                    // Terminal statuses (COMPLETE, PR_OPENED) mean the work was actually
                    // done — also reset to Open for re-assignment (the pair will detect
                    // existing PR/progress).
                    ticket.status = TicketStatus::Open;
                    resolved += 1;
                }
            }
        }
    }
    resolved
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct UnmergedPr {
    pub pr_number: u64,
    pub ticket_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct OrphanedTicket {
    pub ticket_id: String,
    pub worker_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct StaleWorker {
    pub worker_id: String,
    pub ticket_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrashedWorkspace {
    pub workspace_id: String,
    pub worker_id: String,
    pub ticket_id: String,
    pub reason: String,
    pub recovery_attempts: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct CrashedChat {
    pub chat_id: String,
    pub worker_id: String,
    pub ticket_id: String,
    pub reason: String,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FlowRecovery {
    pub unmerged_prs: Vec<UnmergedPr>,
    pub orphaned_tickets: Vec<OrphanedTicket>,
    pub stale_workers: Vec<StaleWorker>,
    pub completed_without_pr: Vec<String>,
    pub crashed_workspaces: Vec<CrashedWorkspace>,
    pub crashed_chats: Vec<CrashedChat>,
    pub has_unmerged_prs: bool,
    pub has_orphaned_tickets: bool,
    pub has_stale_workers: bool,
    pub has_completed_without_pr: bool,
    pub has_crashed_workspaces: bool,
    pub has_crashed_chats: bool,
    pub needs_recovery: bool,
}

pub struct NexusNode {
    pub persona_path: PathBuf,
    pub registry_path: PathBuf,
}

impl NexusNode {
    pub fn new(persona_path: impl Into<PathBuf>, registry_path: impl Into<PathBuf>) -> Self {
        Self {
            persona_path: persona_path.into(),
            registry_path: registry_path.into(),
        }
    }

    fn resolve_github_token(&self) -> Result<String> {
        let registry = self.load_registry()?;
        registry.resolve_github_token("nexus")
    }

    fn load_registry(&self) -> Result<Registry> {
        if self.registry_path.exists() {
            return Registry::load(&self.registry_path);
        }

        if let Ok(path) = std::env::var("OPENFLOWS_REGISTRY_PATH") {
            let path = PathBuf::from(path);
            if path.exists() {
                return Registry::load(path);
            }
        }

        if let Ok(content) = std::env::var("OPENFLOWS_REGISTRY_JSON") {
            let registry: Registry = serde_json::from_str(&content)
                .context("Failed to parse OPENFLOWS_REGISTRY_JSON")?;
            return Ok(registry);
        }

        Registry::load(&self.registry_path)
    }

    async fn sync_issues(&self, store: &SharedStore, owner: &str, repo_name: &str) -> Result<()> {
        if owner.is_empty() || repo_name.is_empty() {
            return Ok(());
        }

        let token = match self.resolve_github_token() {
            Ok(t) => t,
            Err(_) => {
                warn!("GitHub token not configured, skipping issue sync");
                return Ok(());
            }
        };

        let client = github::GithubRestClient::new(&token);
        let gh_issues = match client.list_open_issues(owner, repo_name).await {
            Ok(issues) => issues,
            Err(e) => {
                warn!(error = %e, "GitHub API request failed during issue sync");
                return Ok(());
            }
        };

        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();

        for issue in &gh_issues {
            if issue.pull_request.is_some() {
                continue;
            }

            let ticket_id = format!("T-{:03}", issue.number);
            if tickets.iter().any(|t| t.id == ticket_id) {
                continue;
            }

            info!(ticket_id, title = %issue.title, "Synced new ticket from GitHub issue");

            tickets.push(Ticket {
                id: ticket_id,
                title: issue.title.clone(),
                body: issue.body.clone().unwrap_or_default(),
                priority: 0,
                branch: None,
                status: TicketStatus::Open,
                issue_url: Some(issue.html_url.clone()),
                attempts: 0,
            });
        }

        store.set(KEY_TICKETS, json!(tickets)).await;
        Ok(())
    }

    async fn sync_open_prs(&self, store: &SharedStore, owner: &str, repo_name: &str) -> Result<()> {
        if owner.is_empty() || repo_name.is_empty() {
            return Ok(());
        }

        let token = match self.resolve_github_token() {
            Ok(t) => t,
            Err(_) => {
                warn!("GitHub token not configured, skipping PR sync");
                return Ok(());
            }
        };

        let client = github::GithubRestClient::new(&token);
        let gh_prs = match client.list_open_prs(owner, repo_name).await {
            Ok(prs) => prs,
            Err(e) => {
                warn!(error = %e, "Failed to fetch open PRs from GitHub");
                return Ok(());
            }
        };

        let mut pending_prs: Vec<Value> =
            store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();

        let known_numbers: Vec<u64> = pending_prs
            .iter()
            .filter_map(|p| p["number"].as_u64())
            .collect();

        let mut new_prs = Vec::new();
        let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();

        for pr in &gh_prs {
            if !known_numbers.contains(&pr.number) {
                if let Some(ref tid) = pr.ticket_id {
                    let already_tracked = pending_prs
                        .iter()
                        .any(|p| p["ticket_id"].as_str() == Some(tid.as_str()));
                    if already_tracked {
                        info!(
                            pr_number = pr.number,
                            ticket_id = %tid,
                            "Duplicate PR for ticket already in pending_prs — skipping (only one PR per ticket tracked)"
                        );
                        continue;
                    }

                    if let Some(ticket) = tickets.iter().find(|t| t.id == *tid) {
                        if matches!(ticket.status, TicketStatus::AwaitingHuman { .. }) {
                            info!(
                                pr_number = pr.number,
                                ticket_id = %tid,
                                "Skipping re-add of PR for ticket awaiting human intervention"
                            );
                            continue;
                        }
                        if let TicketStatus::Failed { reason, .. } = &ticket.status {
                            if reason.contains("Merge conflicts")
                                || reason.contains("merge conflict")
                                || reason.contains("conflict rework")
                                || reason.contains("CI failed")
                                || reason.contains("CI timed out")
                                || reason.contains("no worker available for fix")
                                || reason.contains("fix attempts")
                                || reason.contains("See blockers")
                            {
                                info!(
                                    pr_number = pr.number,
                                    ticket_id = %tid,
                                    "Skipping re-add of PR for ticket with CI or conflict failure — worker will be assigned for rework"
                                );
                                continue;
                            }
                        }
                        if matches!(ticket.status, TicketStatus::InProgress { .. }) {
                            info!(
                                pr_number = pr.number,
                                ticket_id = %tid,
                                "Skipping re-add of PR for ticket with InProgress status — CI fix already in flight"
                            );
                            continue;
                        }
                    }
                }

                // Check CI fix attempt counter for ALL PRs (with or without ticket_id).
                // If a PR has exceeded the CI fix attempt limit, skip re-adding it
                // to prevent infinite CI fix loops that burn API tokens.
                {
                    let ci_fix_key = format!("_ci_fix_attempts_{}", pr.number);
                    let ci_fix_attempts: u32 = store.get_typed(&ci_fix_key).await.unwrap_or(0);
                    if ci_fix_attempts >= MAX_CI_FIX_ATTEMPTS_NEXUS {
                        info!(
                            pr_number = pr.number,
                            ticket_id = ?pr.ticket_id,
                            ci_fix_attempts,
                            "Skipping re-add of PR that has exceeded CI fix attempt limit — marking for human intervention"
                        );
                        continue;
                    }
                }

                // For PRs without ticket_id, check if they've exceeded conflict
                // resolution or merge-blocked attempts. This prevents re-adding
                // PRs that are awaiting human intervention or stuck in a loop.
                if pr.ticket_id.is_none() {
                    let conflict_attempts_key = format!("_conflict_attempts_{}", pr.number);
                    let conflict_attempts: u32 =
                        store.get_typed(&conflict_attempts_key).await.unwrap_or(0);
                    let merge_blocked_key = format!("_merge_blocked_{}", pr.number);
                    let merge_blocked_attempts: u32 =
                        store.get_typed(&merge_blocked_key).await.unwrap_or(0);
                    if conflict_attempts >= MAX_CONFLICT_RESOLUTION_ATTEMPTS
                        || merge_blocked_attempts >= MAX_CONFLICT_RESOLUTION_ATTEMPTS
                    {
                        info!(
                            pr_number = pr.number,
                            conflict_attempts,
                            merge_blocked_attempts,
                            "Skipping re-add of PR that has exceeded conflict/merge-blocked attempts — awaiting human intervention"
                        );
                        continue;
                    }
                }

                info!(
                    pr_number = pr.number,
                    ticket_id = ?pr.ticket_id,
                    title = %pr.title,
                    "Discovered untracked open PR on GitHub — adding to pending_prs"
                );
                new_prs.push(pr);
                pending_prs.push(json!({
                    "number": pr.number,
                    "ticket_id": pr.ticket_id,
                    "head_sha": pr.head_sha,
                    "head_branch": pr.head_branch,
                    "base_branch": pr.base_branch,
                    "title": pr.title,
                    "mergeable": pr.mergeable,
                    "has_conflicts": pr.has_conflicts(),
                }));
            }
        }

        let before_count = pending_prs.len();
        pending_prs.retain(|p| {
            let pr_num = p["number"].as_u64().unwrap_or(0);
            if pr_num == 0 {
                return false;
            }
            let still_open = gh_prs.iter().any(|gh| gh.number == pr_num);
            if !still_open {
                info!(
                    pr_number = pr_num,
                    "PR no longer open on GitHub — removing from pending_prs"
                );
            }
            still_open
        });

        let prs_changed =
            pending_prs.len() != known_numbers.len() || pending_prs.len() != before_count;

        if prs_changed {
            store.set(KEY_PENDING_PRS, json!(pending_prs)).await;
        }

        if !new_prs.is_empty() {
            let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
            let mut tickets_changed = false;

            for pr in &new_prs {
                if let Some(ref tid) = pr.ticket_id {
                    if let Some(ticket) = tickets.iter_mut().find(|t| t.id == *tid) {
                        match &ticket.status {
                            TicketStatus::Failed { reason, .. } => {
                                if reason.contains("Merge conflicts")
                                    || reason.contains("merge conflict")
                                    || reason.contains("conflict rework")
                                    || reason.contains("CI failed")
                                    || reason.contains("CI timed out")
                                    || reason.contains("no worker available for fix")
                                    || reason.contains("fix attempts")
                                    || reason.contains("See blockers")
                                {
                                    info!(
                                        ticket_id = tid,
                                        pr_number = pr.number,
                                        "Ticket has CI or conflict failure — NOT overriding to Completed, retaining Failed for rework assignment"
                                    );
                                } else {
                                    info!(
                                        ticket_id = tid,
                                        pr_number = pr.number,
                                        old_status = ?ticket.status,
                                        "Ticket has open PR but non-conflict failure — correcting to Completed(pr_opened)"
                                    );
                                    ticket.status = TicketStatus::Completed {
                                        worker_id: String::from("nexus-reconciliation"),
                                        outcome: "pr_opened".to_string(),
                                    };
                                    tickets_changed = true;
                                }
                            }
                            TicketStatus::Open
                            | TicketStatus::Assigned { .. }
                            | TicketStatus::Exhausted { .. } => {
                                info!(
                                    ticket_id = tid,
                                    pr_number = pr.number,
                                    old_status = ?ticket.status,
                                    "Ticket has open PR but inconsistent status — correcting to Completed(pr_opened)"
                                );
                                ticket.status = TicketStatus::Completed {
                                    worker_id: String::from("nexus-reconciliation"),
                                    outcome: "pr_opened".to_string(),
                                };
                                tickets_changed = true;
                            }
                            TicketStatus::InProgress { .. } => {
                                info!(
                                    ticket_id = tid,
                                    pr_number = pr.number,
                                    "Ticket has open PR but is InProgress (CI fix in flight) — NOT overriding to Completed"
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }

            if tickets_changed {
                store.set(KEY_TICKETS, json!(tickets)).await;
            }
        }

        Ok(())
    }

    async fn load_persona(&self) -> Result<AgentPersona> {
        let content = tokio::fs::read_to_string(&self.persona_path)
            .await
            .map_err(|e| {
                anyhow::anyhow!(
                    "Failed to load nexus persona from {:?}: {}. \
                     Ensure the orchestration/agent/agents/ directory with .agent.md files \
                     is installed alongside the binary or in OPENFLOWS_HOME.",
                    self.persona_path,
                    e
                )
            })?;
        Ok(AgentPersona {
            id: "nexus".to_string(),
            role: "orchestrator".to_string(),
            system_prompt: content,
        })
    }

    async fn sync_registry(&self, store: &SharedStore) -> Result<()> {
        let registry = match self.load_registry() {
            Ok(registry) => registry,
            Err(e) => {
                warn!(error = %e, "Unable to load registry for sync");
                return Ok(());
            }
        };
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let mut changed = false;
        let all_slot_ids = registry.all_worker_slots();

        // Remove slots for workers that are no longer in the registry
        let current_ids: std::collections::HashSet<&str> =
            all_slot_ids.iter().map(|s| s.as_str()).collect();
        let to_remove: Vec<String> = slots
            .keys()
            .filter(|k| !current_ids.contains(k.as_str()))
            .cloned()
            .collect();
        for id in to_remove {
            info!(slot = %id, "Removing worker slot no longer in registry");
            slots.remove(&id);
            changed = true;
        }

        for slot_id in &all_slot_ids {
            let provider = registry
                .resolve_workspace_provider(slot_id)
                .unwrap_or(config::WorkspaceProvider::Local);

            match slots.get_mut(slot_id) {
                Some(slot) => {
                    // Update provider if it changed
                    if slot.workspace_provider != provider {
                        info!(slot = %slot_id, old_provider = ?slot.workspace_provider, new_provider = ?provider, "Updating workspace provider for existing slot");
                        slot.workspace_provider = provider.clone();
                        slot.workspace_id = None;
                        slot.status = WorkerStatus::Idle;
                        changed = true;
                    }
                }
                None => {
                    info!(slot = %slot_id, provider = ?provider, "Adding new worker slot from registry");
                    slots.insert(
                        slot_id.clone(),
                        WorkerSlot {
                            id: slot_id.clone(),
                            status: WorkerStatus::Idle,
                            workspace_id: None,
                            workspace_provider: provider,
                        },
                    );
                    changed = true;
                }
            }
        }

        if changed {
            store.set(KEY_WORKER_SLOTS, json!(slots)).await;
        }

        Ok(())
    }

    async fn coder_client_from_store(store: &SharedStore) -> Option<CoderClient> {
        let coder_url: Option<String> = store
            .get_typed("coder_url")
            .await
            .or_else(|| std::env::var("CODER_URL").ok());
        let coder_token: Option<String> = store
            .get_typed("coder_api_token")
            .await
            .or_else(|| std::env::var("CODER_API_TOKEN").ok());
        match (coder_url, coder_token) {
            (Some(url), Some(token)) if !url.is_empty() && !token.is_empty() => {
                Some(CoderClient::new(&url, &token))
            }
            _ => None,
        }
    }

    async fn provision_coder_workspace(
        &self,
        store: &SharedStore,
        worker_id: &str,
        ticket_id: &str,
    ) -> Result<Option<String>> {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let (workspace_provider, existing_workspace_id) = match slots.get(worker_id) {
            Some(slot) => (slot.workspace_provider.clone(), slot.workspace_id.clone()),
            None => return Ok(None),
        };

        if workspace_provider != WorkspaceProvider::Coder {
            return Ok(existing_workspace_id);
        }
        if let Some(ref existing) = existing_workspace_id {
            // Re-verify that the existing workspace is actually ready before
            // treating it as provisioned.  If readiness was never confirmed
            // (e.g. the previous attempt timed out and persisted the ID
            // optimistically), this re-check prevents an unready workspace
            // from being silently treated as ready.
            if let Some(client) = Self::coder_client_from_store(store).await {
                match client
                    .wait_for_workspace_ready(existing, std::time::Duration::from_secs(180))
                    .await
                {
                    Ok(()) => {
                        info!(
                            worker_id,
                            workspace_id = %existing,
                            "Existing Coder workspace verified ready"
                        );
                    }
                    Err(e) => {
                        warn!(
                            worker_id,
                            workspace_id = %existing,
                            error = %e,
                            "Existing Coder workspace not ready — clearing stale workspace_id"
                        );
                        // Remove the stale ID so a fresh workspace can be
                        // created on the next attempt.
                        if let Some(slot) = slots.get_mut(worker_id) {
                            slot.workspace_id = None;
                        }
                        store
                            .set(KEY_WORKER_SLOTS, serde_json::to_value(&slots)?)
                            .await;
                        return Err(anyhow::anyhow!(
                            "Coder workspace {} not ready on re-check: {}",
                            existing, e
                        ));
                    }
                }
            }
            return Ok(Some(existing.clone()));
        }

        let client = match Self::coder_client_from_store(store).await {
            Some(client) => client,
            None => {
                warn!(
                    worker_id,
                    "Coder workspace requested but CODER_URL/token are unavailable"
                );
                return Ok(None);
            }
        };

        let repository: Option<String> = store.get_typed("repository").await;
        let repo_url = repository
            .as_deref()
            .filter(|s| !s.is_empty())
            .map(|repo| format!("https://github.com/{}.git", repo))
            .unwrap_or_default();
        let template_name = Self::template_name_for_worker(worker_id);
        let workspace_name = Self::workspace_name_for_ticket(worker_id, ticket_id);

        info!(
            worker_id,
            ticket_id, template_name, "Provisioning Coder workspace for worker"
        );

        let workspace = client
            .create_workspace(&CreateWorkspaceRequest {
                template_name,
                name: workspace_name,
                parameters: json!({ "repo_url": repo_url }),
            })
            .await?;

        // Persist the workspace ID immediately so that even if readiness
        // polling times out, retries can reuse the same workspace rather
        // than creating duplicates.
        if let Some(slot) = slots.get_mut(worker_id) {
            slot.workspace_id = Some(workspace.id.clone());
        }
        store
            .set(KEY_WORKER_SLOTS, serde_json::to_value(&slots)?)
            .await;

        // Retry workspace readiness up to 3 attempts, extending the timeout
        // each time.  Coder workspaces can take a while to provision
        // (especially on resource-constrained hosts).
        let max_ready_attempts: u32 = 3;
        let base_ready_timeout_secs: u64 = 180;
        for attempt in 1..=max_ready_attempts {
            let timeout = std::time::Duration::from_secs(base_ready_timeout_secs);
            info!(
                worker_id,
                workspace_id = %workspace.id,
                attempt,
                max_attempts = max_ready_attempts,
                timeout_secs = timeout.as_secs(),
                "Waiting for Coder workspace to become ready"
            );
            match client.wait_for_workspace_ready(&workspace.id, timeout).await {
                Ok(()) => {
                    break;
                }
                Err(e) => {
                    warn!(
                        worker_id,
                        workspace_id = %workspace.id,
                        attempt,
                        max_attempts = max_ready_attempts,
                        error = %e,
                        "Workspace not ready within timeout — will retry"
                    );
                    if attempt == max_ready_attempts {
                        // Last attempt failed — return an error so the caller
                        // can decide how to handle it (e.g. mark ticket as
                        // blocked rather than silently falling back).
                        return Err(anyhow::anyhow!(
                            "Coder workspace {} did not become ready after {} attempts ({}s each): {}",
                            workspace.id, max_ready_attempts,
                            base_ready_timeout_secs, e
                        ));
                    }
                    // Brief pause before retry
                    tokio::time::sleep(std::time::Duration::from_secs(10)).await;
                }
            }
        }

        // Wait for SSH to be available before provisioning configuration.
        // A workspace can report "running" before the agent's SSH daemon is
        // ready to accept connections, leading to timeouts on `coder ssh`
        // commands during pair provisioning.
        // Retry SSH readiness with the same patience.
        let max_ssh_attempts: u32 = 3;
        let base_ssh_timeout_secs: u64 = 120;
        for attempt in 1..=max_ssh_attempts {
            let timeout = std::time::Duration::from_secs(base_ssh_timeout_secs);
            match client.wait_for_workspace_ssh(&workspace.id, timeout).await {
                Ok(()) => break,
                Err(e) => {
                    warn!(
                        worker_id,
                        workspace_id = %workspace.id,
                        attempt,
                        max_attempts = max_ssh_attempts,
                        error = %e,
                        "Workspace SSH not ready within timeout — will retry"
                    );
                    if attempt == max_ssh_attempts {
                        warn!(
                            worker_id,
                            workspace_id = %workspace.id,
                            "Workspace SSH not ready after {} attempts; continuing anyway — \
                             exec operations may fail until SSH becomes available",
                            max_ssh_attempts
                        );
                        break;
                    }
                    tokio::time::sleep(std::time::Duration::from_secs(5)).await;
                }
            }
        }

        info!(
            worker_id,
            workspace_id = %workspace.id,
            "Coder workspace provisioned"
        );
        Ok(Some(workspace.id))
    }

    /// Destroy a Coder workspace and archive all associated chats.
    ///
    /// Used during merge/cleanup to tear down ephemeral workspaces.
    /// Archives chats via `archive_ticket_chats()` before destroying the workspace.
    async fn destroy_coder_workspace(&self, store: &SharedStore, workspace_id: &str) -> Result<()> {
        let client = match Self::coder_client_from_store(store).await {
            Some(client) => client,
            None => {
                warn!(
                    workspace_id,
                    "No Coder client available to destroy workspace"
                );
                return Ok(());
            }
        };

        // Archive all chats associated with this workspace
        let chats = client.list_chats().await.unwrap_or_default();
        let ws_chats: Vec<_> = chats
            .iter()
            .filter(|c| c.workspace_id == workspace_id)
            .collect();

        let mut archived = 0;
        for chat in &ws_chats {
            if client.archive_chat(&chat.id).await.is_ok() {
                archived += 1;
            }
        }

        if !ws_chats.is_empty() {
            info!(
                workspace_id,
                archived,
                total = ws_chats.len(),
                "Archived chats before workspace destruction"
            );
        }

        // Delete the workspace
        client
            .delete_workspace(workspace_id)
            .await
            .context("Failed to delete Coder workspace")?;

        // Clear the workspace_id from the associated slot
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        for slot in slots.values_mut() {
            if slot.workspace_id.as_deref() == Some(workspace_id) {
                slot.workspace_id = None;
            }
        }
        store
            .set(KEY_WORKER_SLOTS, serde_json::to_value(slots)?)
            .await;

        info!(workspace_id, "Destroyed Coder workspace");
        Ok(())
    }

    /// Build a workspace name following the `{role}-{ticket_id}` convention.
    /// ticket_id already includes the "T-" prefix (e.g., "T-041"), so we don't add another one.
    fn workspace_name_for_ticket(worker_id: &str, ticket_id: &str) -> String {
        let role = Self::worker_role(worker_id);
        format!("{}-{}", role, ticket_id)
    }

    /// Resolve the template name for a worker role.
    fn template_name_for_worker(worker_id: &str) -> String {
        let role = Self::worker_role(worker_id);
        let env_key = format!(
            "CODER_{}_TEMPLATE",
            role.to_ascii_uppercase().replace('-', "_")
        );
        std::env::var(&env_key).unwrap_or_else(|_| format!("openflows-{}", role))
    }

    /// Create a Coder Chat for a ticket assignment and store the chat ID in SharedStore.
    ///
    /// This is called after workspace provisioning to set up the chat-driven workflow.
    /// The chat ID is stored at `ticket:{ticket_id}:chat:{worker_id}` so Nexus can
    /// monitor it during reconciliation.
    async fn create_chat_for_assignment(
        &self,
        store: &SharedStore,
        worker_id: &str,
        ticket_id: &str,
    ) {
        let client = match Self::coder_client_from_store(store).await {
            Some(c) => c,
            None => {
                debug!(
                    worker_id,
                    ticket_id, "No Coder client available, skipping chat creation"
                );
                return;
            }
        };

        let slots: HashMap<String, WorkerSlot> = match store.get_typed(KEY_WORKER_SLOTS).await {
            Some(s) => s,
            None => return,
        };

        let slot = match slots.get(worker_id) {
            Some(s) => s,
            None => return,
        };

        if slot.workspace_provider != WorkspaceProvider::Coder {
            return;
        }

        let workspace_id = match &slot.workspace_id {
            Some(ws) => ws.clone(),
            None => {
                warn!(
                    worker_id,
                    ticket_id, "Workspace not yet provisioned, skipping chat creation"
                );
                return;
            }
        };

        // Extract role from worker_id (e.g., "forge-1" -> "forge")
        let role = worker_id
            .rsplit_once('-')
            .map(|(base, _)| base)
            .unwrap_or(worker_id);

        // Build the ticket dispatch payload for Forge
        let dispatch_key = full_ticket_key(ticket_id, KEY_TICKET_DISPATCH, role);
        let dispatch_payload = json!({
            "ticket_id": ticket_id,
            "worker_id": worker_id,
            "chat_id": "pending", // Will be updated after chat creation
            "workspace_id": workspace_id,
            "role": role,
        });

        let chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, role);
        let action_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, role);

        let existing_chat_id: Option<String> = store.get_typed(&chat_key).await;
        if let Some(existing_chat_id) = existing_chat_id {
            match client.get_chat(&existing_chat_id).await {
                Ok(chat) => {
                    if chat.status() == ChatStatus::Waiting {
                        let last_action: Option<String> = store.get_typed(&action_key).await;
                        if matches!(last_action.as_deref(), None | Some("completed")) {
                            let follow_up_prompt = format!(
                                "Continue work on ticket {} from the latest repository state. \
                                 Review the current branch, the dispatch payload, and pick up where the previous pass left off.",
                                ticket_id
                            );
                            if let Ok(message) = client
                                .send_chat_message(
                                    &chat.id,
                                    vec![coder_client::types::ChatInputPart::text(
                                        follow_up_prompt,
                                    )],
                                )
                                .await
                            {
                                info!(
                                    chat_id = %chat.id,
                                    worker_id,
                                    ticket_id,
                                    message_id = %message.id,
                                    "Sent follow-up message to running Coder chat"
                                );
                                store.set(&action_key, json!("follow_up_sent")).await;
                                return;
                            }
                        }
                    }

                    debug!(
                        chat_id = %chat.id,
                        worker_id,
                        ticket_id,
                        status = ?chat.status(),
                        "Existing Coder chat is already active; no new message needed"
                    );
                    return;
                }
                Err(e) => {
                    warn!(
                        chat_id = %existing_chat_id,
                        worker_id,
                        ticket_id,
                        error = %e,
                        "Existing chat lookup failed; recreating assignment chat"
                    );
                }
            }
        }

        // Create the chat with an initial prompt
        let prompt = format!(
            "Work on ticket {}: Review the dispatch payload and begin implementation.",
            ticket_id
        );

        use coder_client::types::{build_chat_labels, ChatInputPart, CreateChatRequest};
        let labels = build_chat_labels(ticket_id, role, "openflows");

        // Resolve the default organization ID required by the Coder chats API.
        let organization_id = match client.get_default_organization_id().await {
            Ok(id) => Some(id),
            Err(e) => {
                warn!(
                    worker_id,
                    ticket_id,
                    error = %e,
                    "Failed to resolve default organization ID; chat creation may fail"
                );
                None
            }
        };

        let chat_req = CreateChatRequest {
            organization_id,
            workspace_id: workspace_id.clone(),
            model_config_id: None, // Let Coder use its default model
            content: vec![ChatInputPart::text(&prompt)],
            labels: Some(labels),
        };

        match client.create_chat(&chat_req).await {
            Ok(chat) => {
                info!(
                    chat_id = %chat.id,
                    worker_id,
                    ticket_id,
                    "Created Chat for ticket assignment"
                );

                // Store chat ID in SharedStore
                store.set(&chat_key, json!(chat.id)).await;

                // Store chat_action as "created" for tracking
                store.set(&action_key, json!("created")).await;

                // Update dispatch payload with actual chat ID
                let mut updated_dispatch = dispatch_payload.clone();
                updated_dispatch["chat_id"] = json!(chat.id);
                store.set(&dispatch_key, updated_dispatch).await;

                // Store workspace_id mapping
                let ws_key = full_ticket_key(ticket_id, KEY_TICKET_WORKSPACE, role);
                store.set(&ws_key, json!(workspace_id)).await;
            }
            Err(e) => {
                warn!(
                    worker_id,
                    ticket_id,
                    error = %e,
                    "Failed to create Chat for ticket assignment"
                );
            }
        }
    }

    async fn check_ci_readiness(
        &self,
        store: &SharedStore,
        owner: &str,
        repo_name: &str,
    ) -> CiReadiness {
        let current: Option<CiReadiness> = store.get_typed(KEY_CI_READINESS).await;
        if let Some(ref readiness) = current {
            if matches!(readiness, CiReadiness::SetupInProgress) {
                return CiReadiness::SetupInProgress;
            }
        }

        if owner.is_empty() || repo_name.is_empty() {
            return CiReadiness::Ready;
        }

        let token = match self.resolve_github_token() {
            Ok(t) => t,
            Err(_) => {
                warn!("GitHub token not configured, assuming CI is ready");
                return CiReadiness::Ready;
            }
        };

        let client = github::GithubRestClient::new(&token);
        match client.has_workflows(owner, repo_name).await {
            Ok(true) => {
                info!("CI workflows found in repository — CI is ready");
                CiReadiness::Ready
            }
            Ok(false) => {
                info!("No CI workflows found in repository — CI setup required");
                CiReadiness::Missing
            }
            Err(e) => {
                warn!(error = %e, "Failed to check CI workflows, assuming ready");
                CiReadiness::Ready
            }
        }
    }

    /// Post a diagnostic comment on a GitHub issue only if no comment with the
    /// given marker tag already exists. This prevents spamming the same issue
    /// across multiple nexus cycles when assignment consistently fails.
    async fn post_comment_once(
        client: &github::GithubRestClient,
        owner: &str,
        repo: &str,
        issue_number: u64,
        marker: &str,
        comment: &str,
    ) {
        match client
            .issue_has_comment_with_marker(owner, repo, issue_number, marker)
            .await
        {
            Ok(true) => {
                info!(
                    owner,
                    repo, issue_number, "Assignment-failure comment already exists — skipping"
                );
            }
            Ok(false) => {
                if let Err(ce) = client
                    .comment_on_issue(owner, repo, issue_number, comment)
                    .await
                {
                    warn!(error = %ce, "Failed to post assignment-failure comment on issue");
                }
            }
            Err(e) => {
                warn!(
                    error = %e,
                    "Failed to check for existing assignment-failure comment — posting anyway"
                );
                if let Err(ce) = client
                    .comment_on_issue(owner, repo, issue_number, comment)
                    .await
                {
                    warn!(error = %ce, "Failed to post assignment-failure comment on issue");
                }
            }
        }
    }

    /// Sync work assignment to GitHub by assigning the issue to the worker.
    /// The worker's GitHub username is resolved dynamically by calling the GitHub API
    /// (GET /user) with the worker's token, which is more robust than reading a static
    /// field from the agent definition and works across repos where the bot is a member.
    ///
    /// If identity resolution fails, a helpful comment is posted on the issue instead
    /// of silently skipping assignment.
    async fn sync_assignment_to_github(
        &self,
        worker_id: &str,
        ticket_id: &str,
        issue_url: &str,
    ) -> Result<()> {
        let parsed_url = url::Url::parse(issue_url)
            .with_context(|| format!("Invalid issue URL format: {}", issue_url))?;

        let host = parsed_url
            .host_str()
            .ok_or_else(|| anyhow::anyhow!("Missing host in URL"))?;
        if !host.eq_ignore_ascii_case("github.com") {
            anyhow::bail!("URL host must be github.com, got: {}", host);
        }

        let path_segments: Vec<&str> = parsed_url
            .path_segments()
            .map(|s| s.collect::<Vec<_>>())
            .unwrap_or_default();

        if path_segments.len() < 4 {
            anyhow::bail!(
                "Invalid GitHub issue URL path. Expected: /{{owner}}/{{repo}}/issues/{{number}}, got: {}",
                parsed_url.path()
            );
        }

        let issue_type = path_segments[2];
        if issue_type != "issues" && issue_type != "pull" {
            anyhow::bail!(
                "Expected URL path segment 3 to be 'issues' or 'pull', got: {}",
                issue_type
            );
        }

        let owner = path_segments[0];
        let repo = path_segments[1];

        let number_str = path_segments[3].trim_end_matches('/');
        let issue_number: u64 = number_str
            .parse()
            .with_context(|| format!("Could not parse issue number from: {}", number_str))?;

        let nexus_token = match self.resolve_github_token() {
            Ok(t) => t,
            Err(e) => {
                anyhow::bail!("GitHub token not configured for nexus: {}", e);
            }
        };

        let nexus_client = github::GithubRestClient::new(&nexus_token);

        let identity_manager = config::IdentityManager::load(&self.registry_path)
            .context("Failed to load IdentityManager from registry")?;

        let registry = identity_manager
            .registry()
            .context("Failed to read registry for worker token check")?;

        let base_id = registry.normalize_agent_id(worker_id);
        #[allow(clippy::needless_borrow)]
        let worker_entry = registry.get(&base_id);

        let has_dedicated_token = worker_entry
            .as_ref()
            .map(|e| e.github_token_env.is_some())
            .unwrap_or(false);

        if !has_dedicated_token {
            warn!(
                worker_id,
                "Worker has no dedicated github_token_env — cannot safely determine its GitHub identity"
            );
            let comment = format!(
                "<!-- openflows-assignment-failure -->\n\
                 ⚠️ **Could not assign this issue to `{}`** — the agent does not have a dedicated \
                 GitHub token configured. Please add a `github_token_env` field for this agent in \
                 `registry.json` so its identity can be resolved dynamically.",
                worker_id
            );
            Self::post_comment_once(
                &nexus_client,
                owner,
                repo,
                issue_number,
                ASSIGNMENT_FAILURE_MARKER,
                &comment,
            )
            .await;
            return Ok(());
        }

        let worker_token_result = identity_manager.resolve_github_token(worker_id);
        if let Err(e) = &worker_token_result {
            warn!(
                worker_id,
                error = %e,
                "Failed to resolve GitHub token for worker"
            );
            let env_var_name = worker_entry
                .as_ref()
                .and_then(|e| e.github_token_env.as_deref())
                .unwrap_or("<missing>");
            let comment = format!(
                "<!-- openflows-assignment-failure -->\n\
                 ⚠️ **Could not assign this issue to `{}`** — the agent's GitHub token environment \
                 variable is not set. Please check that `{}` is configured in the environment.",
                worker_id, env_var_name
            );
            Self::post_comment_once(
                &nexus_client,
                owner,
                repo,
                issue_number,
                ASSIGNMENT_FAILURE_MARKER,
                &comment,
            )
            .await;
            return Ok(());
        }

        let worker_token = worker_token_result.unwrap();
        let worker_client = github::GithubRestClient::new(&worker_token);
        let username_result = worker_client.get_authenticated_user_login().await;
        if let Err(e) = &username_result {
            warn!(
                worker_id,
                error = %e,
                "Failed to resolve GitHub username from worker token"
            );
            let comment = format!(
                "<!-- openflows-assignment-failure -->\n\
                 ⚠️ **Could not assign this issue to `{}`** — failed to look up the agent's GitHub \
                 identity via the API. This usually means the agent's GitHub token is invalid or \
                 expired.\n\nError: {}",
                worker_id, e
            );
            Self::post_comment_once(
                &nexus_client,
                owner,
                repo,
                issue_number,
                ASSIGNMENT_FAILURE_MARKER,
                &comment,
            )
            .await;
            return Ok(());
        }

        let github_username = username_result.unwrap();

        let (assignee_display, assignment_success) = match nexus_client
            .assign_issue(owner, repo, issue_number, &github_username)
            .await
        {
            Ok(_) => (github_username.clone(), true),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.starts_with("Validation failed (422)") {
                    warn!(
                        worker_id,
                        ticket_id,
                        github_username,
                        error = %e,
                        "GitHub user '{}' is not a valid assignee for this repository",
                        github_username
                    );
                    let comment = format!(
                            "<!-- openflows-assignment-failure -->\n\
                             ⚠️ **Could not assign this issue to `@{}`** — this GitHub user is not a \
                             collaborator on `{}/{}`. To fix this, add `{}` as a collaborator or \
                             adjust repository permissions.",
                            github_username, owner, repo, github_username
                        );
                    Self::post_comment_once(
                        &nexus_client,
                        owner,
                        repo,
                        issue_number,
                        ASSIGNMENT_FAILURE_MARKER,
                        &comment,
                    )
                    .await;
                    (github_username.clone(), false)
                } else {
                    return Err(e);
                }
            }
        };

        if assignment_success {
            info!(
                worker_id,
                ticket_id,
                assignee = assignee_display,
                "Successfully synced assignment to GitHub"
            );
        }

        Ok(())
    }

    fn ensure_ci_setup_ticket(
        &self,
        _store: &SharedStore,
        tickets: &mut Vec<Ticket>,
        readiness: &CiReadiness,
    ) {
        if !matches!(readiness, CiReadiness::Missing) {
            return;
        }

        if has_ci_setup_ticket(tickets) {
            info!("CI setup ticket already exists, skipping injection");
            return;
        }

        info!("Injecting CI setup ticket — must be completed before any other work");

        tickets.push(Ticket {
            id: CI_SETUP_TICKET_ID.to_string(),
            title: "CI: Setup GitHub Actions workflows".to_string(),
            body: "This repository has no CI/CD workflows. Create `.github/workflows/ci.yml` \
                   with build, test, and lint checks before any other work proceeds. \
                   Without CI, VESSEL cannot validate PRs and the merge pipeline stalls."
                .to_string(),
            priority: 0,
            branch: None,
            status: TicketStatus::Open,
            issue_url: None,
            attempts: 0,
        });
    }

    fn prioritize_ci_first(tickets: &mut [Ticket]) {
        tickets.sort_by(|a, b| {
            let a_is_ci = is_ci_setup_ticket(a) as u8;
            let b_is_ci = is_ci_setup_ticket(b) as u8;
            b_is_ci
                .cmp(&a_is_ci)
                .then_with(|| a.priority.cmp(&b.priority))
        });
    }

    async fn recover_orphans(store: &SharedStore) -> Result<()> {
        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let mut changed_tickets = false;
        let mut changed_slots = false;

        for ticket in tickets.iter_mut() {
            match &ticket.status {
                TicketStatus::Assigned { worker_id } | TicketStatus::InProgress { worker_id } => {
                    let worker_idle = slots
                        .get(worker_id)
                        .is_none_or(|s| matches!(s.status, WorkerStatus::Idle));
                    let worker_missing = !slots.contains_key(worker_id);
                    if worker_idle || worker_missing {
                        info!(
                            ticket_id = ticket.id,
                            worker_id, "Recovering orphaned ticket — resetting to Open"
                        );
                        ticket.status = TicketStatus::Open;
                        changed_tickets = true;
                    }
                }
                _ => {}
            }
        }

        for slot in slots.values_mut() {
            match &slot.status {
                WorkerStatus::Suspended { ticket_id, .. } => {
                    let ticket_done = tickets.iter().any(|t| {
                        t.id == *ticket_id
                            && matches!(
                                t.status,
                                TicketStatus::Completed { .. } | TicketStatus::Merged { .. }
                            )
                    });
                    if ticket_done {
                        info!(
                            worker_id = slot.id,
                            ticket_id,
                            "Recovering stale worker — ticket completed, recycling to Idle"
                        );
                        slot.status = WorkerStatus::Idle;
                        changed_slots = true;
                    }
                }
                WorkerStatus::Assigned { ticket_id, .. }
                | WorkerStatus::Working { ticket_id, .. } => {
                    let ticket_open = tickets
                        .iter()
                        .any(|t| t.id == *ticket_id && matches!(t.status, TicketStatus::Open));
                    if ticket_open {
                        info!(
                            worker_id = slot.id,
                            ticket_id,
                            "Recovering stale worker — ticket reset to Open, recycling to Idle"
                        );
                        slot.status = WorkerStatus::Idle;
                        changed_slots = true;
                    }
                }
                _ => {}
            }
        }

        if changed_tickets {
            store.set(KEY_TICKETS, json!(tickets)).await;
        }
        if changed_slots {
            store
                .set(KEY_WORKER_SLOTS, serde_json::to_value(slots)?)
                .await;
        }

        Ok(())
    }

    fn reconcile(
        tickets: &[Ticket],
        worker_slots: &HashMap<String, WorkerSlot>,
        pending_prs: &[Value],
    ) -> FlowRecovery {
        let mut recovery = FlowRecovery::default();

        for pr in pending_prs {
            if let Some(obj) = pr.as_object() {
                let pr_number = obj.get("number").and_then(|v| v.as_u64());
                let ticket_id = obj.get("ticket_id").and_then(|v| v.as_str());
                if let Some(pr_num) = pr_number {
                    recovery.unmerged_prs.push(UnmergedPr {
                        pr_number: pr_num,
                        ticket_id: ticket_id.map(|s| s.to_string()),
                    });
                }
            }
        }

        for ticket in tickets {
            match &ticket.status {
                TicketStatus::Assigned { worker_id } | TicketStatus::InProgress { worker_id } => {
                    let worker_exists = worker_slots.contains_key(worker_id);
                    let worker_idle = worker_slots
                        .get(worker_id)
                        .is_some_and(|s| matches!(s.status, WorkerStatus::Idle));
                    if !worker_exists || worker_idle {
                        recovery.orphaned_tickets.push(OrphanedTicket {
                            ticket_id: ticket.id.clone(),
                            worker_id: worker_id.clone(),
                            reason: if !worker_exists {
                                "worker slot missing".to_string()
                            } else {
                                "worker is idle but ticket still assigned".to_string()
                            },
                        });
                    }
                }
                TicketStatus::Completed { outcome, .. } if outcome == "pr_opened" => {
                    let has_pending = pending_prs
                        .iter()
                        .any(|pr| pr.get("ticket_id").and_then(|v| v.as_str()) == Some(&ticket.id));
                    if !has_pending {
                        recovery.completed_without_pr.push(ticket.id.clone());
                    }
                }
                _ => {}
            }
        }

        for slot in worker_slots.values() {
            match &slot.status {
                WorkerStatus::Assigned { ticket_id, .. }
                | WorkerStatus::Working { ticket_id, .. } => {
                    let ticket_exists = tickets.iter().any(|t| t.id == *ticket_id);
                    if !ticket_exists {
                        recovery.stale_workers.push(StaleWorker {
                            worker_id: slot.id.clone(),
                            ticket_id: ticket_id.clone(),
                            reason: "ticket no longer exists".to_string(),
                        });
                    }
                }
                WorkerStatus::Suspended { ticket_id, .. } => {
                    let ticket_completed = tickets.iter().any(|t| {
                        t.id == *ticket_id
                            && matches!(
                                t.status,
                                TicketStatus::Completed { .. } | TicketStatus::Merged { .. }
                            )
                    });
                    if ticket_completed {
                        recovery.stale_workers.push(StaleWorker {
                            worker_id: slot.id.clone(),
                            ticket_id: ticket_id.clone(),
                            reason: "ticket already completed/merged but worker still suspended"
                                .to_string(),
                        });
                    }
                }
                _ => {}
            }
        }

        recovery.has_unmerged_prs = !recovery.unmerged_prs.is_empty();
        recovery.has_orphaned_tickets = !recovery.orphaned_tickets.is_empty();
        recovery.has_stale_workers = !recovery.stale_workers.is_empty();
        recovery.has_completed_without_pr = !recovery.completed_without_pr.is_empty();
        recovery.has_crashed_workspaces = !recovery.crashed_workspaces.is_empty();
        recovery.has_crashed_chats = !recovery.crashed_chats.is_empty();
        recovery.needs_recovery = recovery.has_unmerged_prs
            || recovery.has_orphaned_tickets
            || recovery.has_stale_workers
            || recovery.has_completed_without_pr
            || recovery.has_crashed_workspaces
            || recovery.has_crashed_chats;

        recovery
    }

    fn ticket_worker_id(ticket: &Ticket) -> Option<&str> {
        match &ticket.status {
            TicketStatus::Assigned { worker_id }
            | TicketStatus::InProgress { worker_id }
            | TicketStatus::Merged { worker_id, .. }
            | TicketStatus::Failed { worker_id, .. }
            | TicketStatus::Completed { worker_id, .. }
            | TicketStatus::Exhausted { worker_id, .. }
            | TicketStatus::AwaitingHuman { worker_id, .. } => Some(worker_id.as_str()),
            _ => None,
        }
    }

    fn worker_role(worker_id: &str) -> &str {
        worker_id
            .rsplit_once('-')
            .map(|(base, _)| base)
            .unwrap_or(worker_id)
    }

    async fn workspace_link_for_worker(
        &self,
        store: &SharedStore,
        worker_id: Option<&str>,
    ) -> String {
        let Some(worker_id) = worker_id else {
            return String::new();
        };

        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let Some(slot) = slots.get(worker_id) else {
            return String::new();
        };
        let Some(workspace_id) = slot.workspace_id.as_deref() else {
            return String::new();
        };

        let coder_url: Option<String> = store.get_typed("coder_url").await;
        let Some(coder_url) = coder_url else {
            return String::new();
        };

        format!(
            "{}/workspaces/{}",
            coder_url.trim_end_matches('/'),
            workspace_id
        )
    }

    async fn notify_awaiting_human(
        &self,
        store: &SharedStore,
        ticket_id: &str,
        worker_id: Option<&str>,
        reason: &str,
        github_link: Option<String>,
    ) {
        let service = NotificationService::from_env();
        let role = worker_id.map(Self::worker_role).unwrap_or("nexus");
        let workspace_link = self.workspace_link_for_worker(store, worker_id).await;
        let msg = NotificationMessage {
            ticket_id: ticket_id.to_string(),
            role: role.to_string(),
            reason: reason.to_string(),
            workspace_link,
            github_link: github_link.unwrap_or_default(),
        };
        service.notify(&msg).await;
    }

    async fn mark_ticket_awaiting_human(
        &self,
        store: &SharedStore,
        ticket_id: &str,
        worker_id: &str,
        reason: &str,
    ) {
        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        let mut github_link: Option<String> = None;

        for ticket in tickets.iter_mut() {
            if ticket.id == ticket_id {
                github_link = ticket.issue_url.clone();
                let attempts = ticket.attempts + 1;
                ticket.attempts = attempts;
                ticket.status = TicketStatus::AwaitingHuman {
                    worker_id: worker_id.to_string(),
                    reason: reason.to_string(),
                    attempts,
                };
                break;
            }
        }

        store.set(KEY_TICKETS, json!(tickets)).await;
        store
            .set(
                &full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS),
                json!("awaiting_human"),
            )
            .await;
        self.notify_awaiting_human(store, ticket_id, Some(worker_id), reason, github_link)
            .await;
    }

    async fn inspect_coder_recovery(
        &self,
        store: &SharedStore,
        tickets: &[Ticket],
        worker_slots: &HashMap<String, WorkerSlot>,
        recovery: &mut FlowRecovery,
    ) -> Result<()> {
        let client = match Self::coder_client_from_store(store).await {
            Some(client) => client,
            None => return Ok(()),
        };

        let chats = match client.list_chats().await {
            Ok(chats) => chats,
            Err(e) => {
                warn!(error = %e, "Failed to list Coder chats for recovery inspection");
                return Ok(());
            }
        };

        for chat in chats {
            let flow = chat.labels.get(CHAT_LABEL_FLOW).and_then(|v| v.as_str());
            if flow != Some("openflows") {
                continue;
            }

            let Some(ticket_id) = chat.labels.get(CHAT_LABEL_TICKET).and_then(|v| v.as_str())
            else {
                continue;
            };
            let role = chat
                .labels
                .get(CHAT_LABEL_ROLE)
                .and_then(|v| v.as_str())
                .unwrap_or("unknown");

            store
                .set(
                    &full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS),
                    json!(chat.status().as_str()),
                )
                .await;

            let action_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, role);
            let last_action: Option<String> = store.get_typed(&action_key).await;
            let worker_id = tickets
                .iter()
                .find(|ticket| ticket.id == ticket_id)
                .and_then(Self::ticket_worker_id)
                .map(str::to_string)
                .unwrap_or_else(|| role.to_string());

            match chat.status() {
                ChatStatus::Error => {
                    recovery.crashed_chats.push(CrashedChat {
                        chat_id: chat.id.clone(),
                        worker_id,
                        ticket_id: ticket_id.to_string(),
                        reason: "chat entered error status".to_string(),
                    });
                }
                ChatStatus::Waiting => {
                    if last_action.as_deref() == Some("interrupted") {
                        recovery.crashed_chats.push(CrashedChat {
                            chat_id: chat.id.clone(),
                            worker_id,
                            ticket_id: ticket_id.to_string(),
                            reason: "chat was interrupted after a workspace crash".to_string(),
                        });
                    } else if !matches!(
                        last_action.as_deref(),
                        Some("follow_up_sent") | Some("completed")
                    ) {
                        store.set(&action_key, json!("completed")).await;
                    }
                }
                _ => {}
            }
        }

        for slot in worker_slots.values() {
            let Some(workspace_id) = slot.workspace_id.as_deref() else {
                continue;
            };
            if slot.workspace_provider != WorkspaceProvider::Coder {
                continue;
            }

            let ticket_id = match &slot.status {
                WorkerStatus::Assigned { ticket_id, .. }
                | WorkerStatus::Working { ticket_id, .. }
                | WorkerStatus::Done { ticket_id, .. }
                | WorkerStatus::Suspended { ticket_id, .. } => ticket_id.clone(),
                WorkerStatus::Idle => String::new(),
            };
            if ticket_id.is_empty() {
                continue;
            }

            let role = Self::worker_role(&slot.id);
            let heartbeat_reason = match store
                .get_typed::<HeartbeatRecord>(&heartbeat_key(role, &ticket_id))
                .await
            {
                Some(heartbeat) => {
                    let now_ms = SystemTime::now()
                        .duration_since(UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_millis() as u64;
                    let age_secs = now_ms.saturating_sub(heartbeat.ts) / 1_000;
                    if heartbeat.status != "running" {
                        Some(format!(
                            "heartbeat status is {} for ws {}",
                            heartbeat.status, heartbeat.ws_id
                        ))
                    } else if age_secs > HEARTBEAT_STALE_AFTER_SECS {
                        Some(format!(
                            "heartbeat stale after {}s for ws {}",
                            age_secs, heartbeat.ws_id
                        ))
                    } else if heartbeat.ws_id != workspace_id {
                        Some(format!(
                            "heartbeat ws mismatch (heartbeat={}, slot={})",
                            heartbeat.ws_id, workspace_id
                        ))
                    } else {
                        None
                    }
                }
                None => None,
            };

            if let Some(reason) = heartbeat_reason {
                let recovery_attempts = store
                    .get_typed::<u32>(&full_ticket_key_flat(
                        &ticket_id,
                        KEY_TICKET_RECOVERY_ATTEMPTS,
                    ))
                    .await
                    .unwrap_or(0);
                recovery.crashed_workspaces.push(CrashedWorkspace {
                    workspace_id: workspace_id.to_string(),
                    worker_id: slot.id.clone(),
                    ticket_id: ticket_id.clone(),
                    reason,
                    recovery_attempts,
                });
                continue;
            }

            let recovery_attempts = if ticket_id.is_empty() {
                0
            } else {
                store
                    .get_typed::<u32>(&full_ticket_key_flat(
                        &ticket_id,
                        KEY_TICKET_RECOVERY_ATTEMPTS,
                    ))
                    .await
                    .unwrap_or(0)
            };

            let reason = match client.get_workspace(workspace_id).await {
                Ok(workspace) => {
                    let workspace_status = workspace.workspace_status();
                    let agent_status = workspace.agent_status();
                    match workspace_status {
                        WorkspaceStatus::Running if agent_status == AgentStatus::Connected => None,
                        WorkspaceStatus::Running => {
                            Some(format!("workspace agent status is {:?}", agent_status))
                        }
                        WorkspaceStatus::Pending => Some("workspace is pending".to_string()),
                        WorkspaceStatus::Starting => Some("workspace is starting".to_string()),
                        WorkspaceStatus::Stopping => Some("workspace is stopping".to_string()),
                        WorkspaceStatus::Stopped => Some("workspace is stopped".to_string()),
                        WorkspaceStatus::Failed => Some("workspace failed".to_string()),
                        WorkspaceStatus::Deleting => Some("workspace is deleting".to_string()),
                        WorkspaceStatus::Deleted => Some("workspace is deleted".to_string()),
                        WorkspaceStatus::Unknown(raw) => {
                            Some(format!("workspace status is {}", raw))
                        }
                    }
                }
                Err(e) => Some(format!("workspace lookup failed: {}", e)),
            };

            if let Some(reason) = reason {
                recovery.crashed_workspaces.push(CrashedWorkspace {
                    workspace_id: workspace_id.to_string(),
                    worker_id: slot.id.clone(),
                    ticket_id,
                    reason,
                    recovery_attempts,
                });
            }
        }

        recovery.has_crashed_workspaces = !recovery.crashed_workspaces.is_empty();
        recovery.has_crashed_chats = !recovery.crashed_chats.is_empty();
        recovery.needs_recovery = recovery.needs_recovery
            || recovery.has_crashed_workspaces
            || recovery.has_crashed_chats;

        Ok(())
    }

    async fn increment_recovery_attempts(&self, store: &SharedStore, ticket_id: &str) -> u32 {
        let key = full_ticket_key_flat(ticket_id, KEY_TICKET_RECOVERY_ATTEMPTS);
        let current: u32 = store.get_typed(&key).await.unwrap_or(0);
        let next = current + 1;
        store.set(&key, json!(next)).await;
        next
    }

    async fn repair_coder_recovery(
        &self,
        store: &SharedStore,
        recovery: &FlowRecovery,
    ) -> Result<()> {
        let client = match Self::coder_client_from_store(store).await {
            Some(client) => client,
            None => return Ok(()),
        };

        for crashed_chat in &recovery.crashed_chats {
            let action_key = full_ticket_key(
                &crashed_chat.ticket_id,
                KEY_TICKET_CHAT_ACTION,
                Self::worker_role(&crashed_chat.worker_id),
            );
            store.set(&action_key, json!("interrupted")).await;

            if let Err(e) = client.interrupt_chat(&crashed_chat.chat_id).await {
                warn!(
                    chat_id = %crashed_chat.chat_id,
                    ticket_id = %crashed_chat.ticket_id,
                    error = %e,
                    "Failed to interrupt crashed chat"
                );
            }
        }

        for crashed_workspace in &recovery.crashed_workspaces {
            if crashed_workspace.ticket_id.is_empty() {
                continue;
            }

            let attempts = self
                .increment_recovery_attempts(store, &crashed_workspace.ticket_id)
                .await;

            if attempts >= Ticket::MAX_ATTEMPTS {
                let reason = format!(
                    "workspace {} crashed {} times and requires human intervention",
                    crashed_workspace.workspace_id, attempts
                );
                self.mark_ticket_awaiting_human(
                    store,
                    &crashed_workspace.ticket_id,
                    &crashed_workspace.worker_id,
                    &reason,
                )
                .await;
                warn!(
                    workspace_id = %crashed_workspace.workspace_id,
                    ticket_id = %crashed_workspace.ticket_id,
                    attempts,
                    "Recovery limit reached — escalating to human intervention"
                );
                continue;
            }

            match client.get_workspace(&crashed_workspace.workspace_id).await {
                Ok(workspace) => match workspace.workspace_status() {
                    WorkspaceStatus::Stopped | WorkspaceStatus::Stopping => {
                        info!(
                            workspace_id = %crashed_workspace.workspace_id,
                            ticket_id = %crashed_workspace.ticket_id,
                            "Restarting stopped Coder workspace"
                        );
                        if let Err(e) = client
                            .start_workspace(&crashed_workspace.workspace_id)
                            .await
                        {
                            warn!(
                                workspace_id = %crashed_workspace.workspace_id,
                                ticket_id = %crashed_workspace.ticket_id,
                                error = %e,
                                "Failed to restart Coder workspace"
                            );
                        }
                    }
                    WorkspaceStatus::Running => {
                        let heartbeat_stale = crashed_workspace.reason.contains("heartbeat");
                        if workspace.agent_status() != AgentStatus::Connected || heartbeat_stale {
                            warn!(
                                workspace_id = %crashed_workspace.workspace_id,
                                ticket_id = %crashed_workspace.ticket_id,
                                agent_status = ?workspace.agent_status(),
                                reason = %crashed_workspace.reason,
                                "Restarting running Coder workspace to recover stale agent/heartbeat"
                            );
                            let _ = client.stop_workspace(&crashed_workspace.workspace_id).await;
                            if let Err(e) = client
                                .start_workspace(&crashed_workspace.workspace_id)
                                .await
                            {
                                warn!(
                                    workspace_id = %crashed_workspace.workspace_id,
                                    ticket_id = %crashed_workspace.ticket_id,
                                    error = %e,
                                    "Failed to restart running Coder workspace"
                                );
                            }
                        }
                    }
                    WorkspaceStatus::Pending
                    | WorkspaceStatus::Starting
                    | WorkspaceStatus::Failed
                    | WorkspaceStatus::Deleting
                    | WorkspaceStatus::Deleted
                    | WorkspaceStatus::Unknown(_) => {
                        info!(
                            workspace_id = %crashed_workspace.workspace_id,
                            ticket_id = %crashed_workspace.ticket_id,
                            status = ?workspace.workspace_status(),
                            "Recreating Coder workspace after crash"
                        );

                        let mut slots: HashMap<String, WorkerSlot> =
                            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                        if let Some(slot) = slots.get_mut(&crashed_workspace.worker_id) {
                            slot.workspace_id = None;
                            store.set(KEY_WORKER_SLOTS, json!(slots)).await;
                        }

                        if let Err(e) = self
                            .provision_coder_workspace(
                                store,
                                &crashed_workspace.worker_id,
                                &crashed_workspace.ticket_id,
                            )
                            .await
                        {
                            warn!(
                                worker_id = %crashed_workspace.worker_id,
                                ticket_id = %crashed_workspace.ticket_id,
                                error = %e,
                                "Failed to recreate Coder workspace"
                            );
                            continue;
                        }

                        self.create_chat_for_assignment(
                            store,
                            &crashed_workspace.worker_id,
                            &crashed_workspace.ticket_id,
                        )
                        .await;
                    }
                },
                Err(e) => {
                    warn!(
                        workspace_id = %crashed_workspace.workspace_id,
                        ticket_id = %crashed_workspace.ticket_id,
                        error = %e,
                        "Could not inspect crashed workspace"
                    );
                }
            }
        }

        Ok(())
    }
}

#[async_trait]
impl Node for NexusNode {
    fn name(&self) -> &str {
        "nexus"
    }

    async fn prep(&self, store: &SharedStore) -> Result<Value> {
        if let Err(e) = self.sync_registry(store).await {
            warn!("Failed to sync registry: {}", e);
        }

        let repository = store.get("repository").await.unwrap_or(json!(""));

        let (owner, repo_name) = repository
            .as_str()
            .and_then(|r| {
                let parts: Vec<&str> = r.split('/').collect();
                if parts.len() == 2 {
                    Some((parts[0].to_string(), parts[1].to_string()))
                } else {
                    None
                }
            })
            .unwrap_or((String::new(), String::new()));

        if let Err(e) = self.sync_issues(store, &owner, &repo_name).await {
            warn!("Failed to sync issues from GitHub: {}", e);
        }

        if let Err(e) = self.sync_open_prs(store, &owner, &repo_name).await {
            warn!("Failed to sync open PRs from GitHub: {}", e);
        }

        let ci_readiness = self.check_ci_readiness(store, &owner, &repo_name).await;
        store.set(KEY_CI_READINESS, json!(ci_readiness)).await;

        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();

        let resolved = auto_resolve_unrecognized_statuses(&mut tickets);
        if resolved > 0 {
            info!(
                resolved,
                "Auto-resolved tickets with unrecognized STATUS.json statuses"
            );
            store.set(KEY_TICKETS, json!(tickets)).await;
        }

        self.ensure_ci_setup_ticket(store, &mut tickets, &ci_readiness);
        Self::prioritize_ci_first(&mut tickets);

        store.set(KEY_TICKETS, json!(tickets)).await;

        let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();

        let has_assignable = tickets.iter().any(|t| t.is_assignable());

        let mut worker_slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let mut recycled = false;
        if has_assignable {
            for slot in worker_slots.values_mut() {
                if matches!(slot.status, WorkerStatus::Done { .. }) {
                    info!(
                        worker_id = slot.id,
                        "Recycling Done worker to Idle — assignable tickets exist"
                    );
                    slot.status = WorkerStatus::Idle;
                    recycled = true;
                }
            }
        }
        if recycled {
            store.set(KEY_WORKER_SLOTS, json!(worker_slots)).await;
        }

        let worker_slots = store.get(KEY_WORKER_SLOTS).await.unwrap_or(json!({}));
        let open_prs = store.get(KEY_PENDING_PRS).await.unwrap_or(json!([]));
        let command_gate = store.get(KEY_COMMAND_GATE).await.unwrap_or(json!({}));

        let pending_prs_vec: Vec<Value> = open_prs.as_array().cloned().unwrap_or_default();
        let worker_slots_map: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let mut recovery = Self::reconcile(&tickets, &worker_slots_map, &pending_prs_vec);
        if let Err(e) = self
            .inspect_coder_recovery(store, &tickets, &worker_slots_map, &mut recovery)
            .await
        {
            warn!(error = %e, "Failed to inspect Coder recovery state");
        }
        if recovery.has_crashed_workspaces || recovery.has_crashed_chats {
            if let Err(e) = self.repair_coder_recovery(store, &recovery).await {
                warn!(error = %e, "Failed to apply Coder recovery actions");
            }
        }

        for (worker_id, slot) in &worker_slots_map {
            if slot.workspace_provider != WorkspaceProvider::Coder {
                continue;
            }
            let ticket_id = match &slot.status {
                WorkerStatus::Assigned { ticket_id, .. }
                | WorkerStatus::Working { ticket_id, .. }
                | WorkerStatus::Suspended { ticket_id, .. } => Some(ticket_id.as_str()),
                _ => None,
            };

            if let Some(ticket_id) = ticket_id {
                self.create_chat_for_assignment(store, worker_id, ticket_id)
                    .await;
            }
        }

        if recovery.needs_recovery {
            info!(
                unmerged_prs = recovery.unmerged_prs.len(),
                orphaned_tickets = recovery.orphaned_tickets.len(),
                stale_workers = recovery.stale_workers.len(),
                completed_without_pr = recovery.completed_without_pr.len(),
                crashed_workspaces = recovery.crashed_workspaces.len(),
                crashed_chats = recovery.crashed_chats.len(),
                "Flow recovery: inconsistencies detected"
            );
        }

        let ci_must_go_first = matches!(ci_readiness, CiReadiness::Missing)
            || (matches!(ci_readiness, CiReadiness::SetupInProgress)
                && ci_setup_ticket_active(&tickets));

        let assignable_tickets: Vec<&Ticket> = if ci_must_go_first {
            tickets
                .iter()
                .filter(|t| is_ci_setup_ticket(t) && t.is_assignable())
                .collect()
        } else {
            tickets.iter().filter(|t| t.is_assignable()).collect()
        };

        Ok(json!({
            "tickets": tickets,
            "assignable_tickets": assignable_tickets,
            "worker_slots": worker_slots,
            "open_prs": open_prs,
            "command_gate": command_gate,
            "repository": repository,
            "owner": owner,
            "repo_name": repo_name,
            "ci_readiness": ci_readiness,
            "ci_must_go_first": ci_must_go_first,
            "flow_recovery": recovery,
        }))
    }

    async fn exec(&self, context: Value) -> Result<Value> {
        info!("Nexus calling AgentRunner for orchestration...");

        let registry = self.load_registry()?;
        let model_backend = registry.get("nexus").and_then(|e| e.model_backend.clone());

        let github_token = self.resolve_github_token()?;

        let mut runner =
            AgentRunner::from_env_with_token(model_backend.as_deref(), &github_token).await?;
        let persona = self.load_persona().await?;

        let decision: AgentDecision = runner.run(&persona, context, 10).await?;

        Ok(json!(decision))
    }

    async fn post(&self, store: &SharedStore, result: Value) -> Result<Action> {
        let decision: AgentDecision = serde_json::from_value(result)?;

        info!(action = %decision.action, notes = %decision.notes, "Nexus decision reached");

        if decision.action == ACTION_MERGE_PRS {
            store.set(KEY_NO_WORK_COUNT, json!(0)).await;

            let pending_prs: Vec<Value> =
                store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();

            if pending_prs.is_empty() {
                let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                let has_assignable = tickets.iter().any(|t| t.is_assignable());
                if has_assignable {
                    info!("merge_prs action but no open PRs — assignable tickets exist, falling through to work assignment");
                } else {
                    info!("merge_prs action but no open PRs and no assignable tickets — no work");
                }
                return Ok(Action::new(ACTION_NO_WORK));
            }

            info!(
                pr_count = pending_prs.len(),
                "Nexus: Routing to VESSEL to merge {} pending PR(s)",
                pending_prs.len()
            );

            return Ok(Action::new(ACTION_MERGE_PRS));
        }

        if decision.action == "work_assigned" {
            store.set(KEY_NO_WORK_COUNT, json!(0)).await;

            Self::recover_orphans(store).await?;

            if let Some(worker_id) = &decision.assign_to {
                if let Some(ticket_id) = &decision.ticket_id {
                    info!(worker_id, ticket_id, "Nexus: Assigning ticket to worker");

                    let mut tickets: Vec<Ticket> =
                        store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                    if let Some(ticket) = tickets.iter_mut().find(|t| t.id == *ticket_id) {
                        ticket.status = TicketStatus::Assigned {
                            worker_id: worker_id.clone(),
                        };
                        if let Some(url) = &decision.issue_url {
                            ticket.issue_url = Some(url.clone());
                        }
                    } else {
                        info!(
                            ticket_id,
                            "Creating new ticket in store from LLM assignment"
                        );
                        tickets.push(Ticket {
                            id: ticket_id.clone(),
                            title: decision.notes.clone(),
                            body: String::new(),
                            priority: 0,
                            branch: None,
                            status: TicketStatus::Assigned {
                                worker_id: worker_id.clone(),
                            },
                            issue_url: decision.issue_url.clone(),
                            attempts: 0,
                        });
                    }
                    store.set(KEY_TICKETS, json!(tickets)).await;

                    if ticket_id.starts_with("T-CI-") {
                        info!("CI setup ticket assigned — marking CI readiness as in-progress");
                        store
                            .set(KEY_CI_READINESS, json!(CiReadiness::SetupInProgress))
                            .await;
                    }

                    let mut slots: HashMap<String, WorkerSlot> =
                        store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                    let mut should_provision_coder = false;
                    if let Some(slot) = slots.get_mut(worker_id) {
                        should_provision_coder =
                            matches!(slot.workspace_provider, WorkspaceProvider::Coder)
                                && slot.workspace_id.is_none();
                        slot.status = WorkerStatus::Assigned {
                            ticket_id: ticket_id.clone(),
                            issue_url: decision.issue_url.clone(),
                        };
                        store
                            .set(KEY_WORKER_SLOTS, serde_json::to_value(slots)?)
                            .await;
                        info!(worker_id, ticket_id, issue_url = ?decision.issue_url, "Nexus: Store updated with NEW worker assignment");
                    }

                    if should_provision_coder {
                        if let Err(e) = self
                            .provision_coder_workspace(store, worker_id, ticket_id)
                            .await
                        {
                            warn!(
                                worker_id,
                                ticket_id,
                                error = %e,
                                "Failed to provision Coder workspace"
                            );
                        }
                    }

                    // Create a Coder Chat for this assignment and record it in SharedStore
                    self.create_chat_for_assignment(store, worker_id, ticket_id)
                        .await;

                    // Sync assignment to GitHub: assign issue, add comment, and label
                    if let Some(issue_url) = &decision.issue_url {
                        if let Err(e) = self
                            .sync_assignment_to_github(worker_id, ticket_id, issue_url)
                            .await
                        {
                            warn!(
                                worker_id,
                                ticket_id,
                                issue_url,
                                error = %e,
                                "Failed to sync assignment to GitHub — continuing anyway"
                            );
                        }
                    }
                }
            }
        }

        if decision.action == "no_work" {
            let count: u32 = store.get_typed(KEY_NO_WORK_COUNT).await.unwrap_or(0);
            let new_count = count + 1;
            store.set(KEY_NO_WORK_COUNT, json!(new_count)).await;

            if new_count >= NO_WORK_THRESHOLD {
                info!(
                    consecutive = new_count,
                    "No work found after {} consecutive checks — stopping", NO_WORK_THRESHOLD
                );
                return Ok(Action::new(STOP_SIGNAL));
            }
        }

        if decision.action == "approve_command" || decision.action == "reject_command" {
            let mut gate: HashMap<String, Value> =
                store.get_typed(KEY_COMMAND_GATE).await.unwrap_or_default();
            if let Some(worker_id) = gate.keys().next().cloned() {
                info!(
                    worker = worker_id,
                    action = decision.action,
                    "CommandGate processing"
                );
                gate.remove(&worker_id);
                store.set(KEY_COMMAND_GATE, json!(gate)).await;

                let mut slots: HashMap<String, WorkerSlot> =
                    store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                if let Some(slot) = slots.get_mut(&worker_id) {
                    if decision.action == "approve_command" {
                        if let WorkerStatus::Suspended {
                            ticket_id,
                            issue_url,
                            ..
                        } = &slot.status
                        {
                            slot.status = WorkerStatus::Assigned {
                                ticket_id: ticket_id.clone(),
                                issue_url: issue_url.clone(),
                            };
                        }
                    } else {
                        slot.status = WorkerStatus::Idle;
                    }
                }
                store.set(KEY_WORKER_SLOTS, json!(slots)).await;
            }
        }

        Ok(Action::new(decision.action))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_remap_unrecognized_status_review_keywords() {
        assert_eq!(
            remap_unrecognized_status("AWAITING_REVIEW"),
            Some("PENDING_REVIEW")
        );
        assert_eq!(
            remap_unrecognized_status("REVIEW_PENDING"),
            Some("PENDING_REVIEW")
        );
        assert_eq!(
            remap_unrecognized_status("WAITING_FOR_APPROVAL"),
            Some("PENDING_REVIEW")
        );
        assert_eq!(remap_unrecognized_status("ON_HOLD"), Some("PENDING_REVIEW"));
        assert_eq!(
            remap_unrecognized_status("SENTINEL_REVIEW_NEEDED"),
            Some("AWAITING_SENTINEL_REVIEW")
        );
    }

    #[test]
    fn test_remap_unrecognized_status_done_keywords() {
        assert_eq!(remap_unrecognized_status("ALL_DONE"), Some("COMPLETE"));
        assert_eq!(
            remap_unrecognized_status("IMPLEMENTATION_COMPLETE"),
            Some("COMPLETE")
        );
        assert_eq!(remap_unrecognized_status("FINISHED_WORK"), Some("COMPLETE"));
    }

    #[test]
    fn test_remap_unrecognized_status_blocked_keywords() {
        assert_eq!(remap_unrecognized_status("BUILD_FAILED"), Some("BLOCKED"));
        assert_eq!(remap_unrecognized_status("ERROR_OCCURRED"), Some("BLOCKED"));
        assert_eq!(
            remap_unrecognized_status("CANNOT_PROCEED_FURTHER"),
            Some("BLOCKED")
        );
    }

    #[test]
    fn test_remap_unrecognized_status_pr_keywords() {
        assert_eq!(
            remap_unrecognized_status("PR_OPEN_PENDING"),
            Some("PR_OPENED")
        );
        assert_eq!(
            remap_unrecognized_status("PULL_REQUEST_CREATED"),
            Some("PR_OPENED")
        );
    }

    #[test]
    fn test_remap_unrecognized_status_fuel_keywords() {
        assert_eq!(
            remap_unrecognized_status("BUDGET_EXCEEDED"),
            Some("FUEL_EXHAUSTED")
        );
        assert_eq!(
            remap_unrecognized_status("FUEL_DEPLETED"),
            Some("FUEL_EXHAUSTED")
        );
    }

    #[test]
    fn test_remap_unrecognized_status_no_match() {
        assert_eq!(remap_unrecognized_status("MYSTERY"), None);
        assert_eq!(remap_unrecognized_status("GIBBERISH"), None);
    }

    #[test]
    fn test_auto_resolve_unrecognized_statuses() {
        let mut tickets = vec![
            Ticket {
                id: "T-001".to_string(),
                title: "Test ticket".to_string(),
                body: String::new(),
                priority: 0,
                branch: None,
                issue_url: None,
                attempts: 0,
                status: TicketStatus::Failed {
                    worker_id: "forge-1".to_string(),
                    reason: "Unrecognized STATUS.json status: AWAITING_REVIEW (normalized: AWAITING_REVIEW)".to_string(),
                    attempts: 1,
                },
            },
            Ticket {
                id: "T-002".to_string(),
                title: "Other ticket".to_string(),
                body: String::new(),
                priority: 0,
                branch: None,
                issue_url: None,
                attempts: 0,
                status: TicketStatus::Failed {
                    worker_id: "forge-2".to_string(),
                    reason: "fuel_exhausted".to_string(),
                    attempts: 1,
                },
            },
        ];

        let resolved = auto_resolve_unrecognized_statuses(&mut tickets);
        assert_eq!(resolved, 1);
        assert!(matches!(tickets[0].status, TicketStatus::Open));
        assert!(matches!(tickets[1].status, TicketStatus::Failed { .. }));
    }
}
