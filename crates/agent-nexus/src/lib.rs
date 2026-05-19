// crates/agent-nexus/src/lib.rs
use agent_client::{AgentDecision, AgentPersona, AgentRunner};
use anyhow::Result;
use async_trait::async_trait;
use config::{
    state::{KEY_COMMAND_GATE, KEY_PENDING_PRS, KEY_TICKETS, KEY_WORKER_SLOTS},
    AgentDef, Registry, Ticket, TicketStatus, WorkerSlot, WorkerStatus, ACTION_MERGE_PRS,
    ACTION_NO_WORK,
};
use pocketflow_core::{node::STOP_SIGNAL, Action, Node, SharedStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{info, warn};

const NO_WORK_THRESHOLD: u32 = 3;
const KEY_NO_WORK_COUNT: &str = "_no_work_count";
const KEY_CI_READINESS: &str = "ci_readiness";
const MAX_CONFLICT_RESOLUTION_ATTEMPTS: u32 = 3;
const CI_SETUP_TICKET_ID: &str = "T-CI-001";

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
pub struct FlowRecovery {
    pub unmerged_prs: Vec<UnmergedPr>,
    pub orphaned_tickets: Vec<OrphanedTicket>,
    pub stale_workers: Vec<StaleWorker>,
    pub completed_without_pr: Vec<String>,
    pub has_unmerged_prs: bool,
    pub has_orphaned_tickets: bool,
    pub has_stale_workers: bool,
    pub has_completed_without_pr: bool,
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
        let registry = Registry::load(&self.registry_path)?;
        registry.resolve_github_token("nexus")
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
        let content = tokio::fs::read_to_string(&self.persona_path).await?;
        Ok(AgentPersona {
            id: "nexus".to_string(),
            role: "orchestrator".to_string(),
            system_prompt: content,
        })
    }

    async fn sync_registry(&self, store: &SharedStore) -> Result<()> {
        if !self.registry_path.exists() {
            return Ok(());
        }

        let registry = Registry::load(&self.registry_path)?;
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let mut changed = false;

        for slot_id in registry.all_worker_slots() {
            if !slots.contains_key(&slot_id) {
                info!(slot = slot_id, "Adding new worker slot from registry");
                slots.insert(
                    slot_id.clone(),
                    WorkerSlot {
                        id: slot_id,
                        status: WorkerStatus::Idle,
                    },
                );
                changed = true;
            }
        }

        if changed {
            store.set(KEY_WORKER_SLOTS, json!(slots)).await;
        }

        Ok(())
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

    /// Sync work assignment to GitHub by assigning the issue to the forge worker,
    /// adding a comment indicating assignment, and optionally adding labels.
    /// The forge worker's GitHub username is loaded from the agent definition (forge.agent.md).
    async fn sync_assignment_to_github(
        &self,
        worker_id: &str,
        ticket_id: &str,
        issue_url: &str,
    ) -> Result<()> {
        // Parse owner/repo from issue URL and extract issue number
        // Expected format: https://github.com/{owner}/{repo}/issues/{number}
        let url_parts: Vec<&str> = issue_url.split('/').collect();
        if url_parts.len() < 2 {
            anyhow::bail!("Invalid issue URL format: {}", issue_url);
        }

        // Extract owner, repo, and issue number from URL
        let repo_idx = url_parts.iter().position(|&p| p == "github.com");
        let (owner, repo, issue_number) = match repo_idx {
            Some(idx) if url_parts.len() > idx + 3 => {
                let owner = url_parts[idx + 1];
                let repo = url_parts[idx + 2];
                // Issue number is the last component
                let number = url_parts
                    .last()
                    .and_then(|n| n.parse::<u64>().ok())
                    .ok_or_else(|| anyhow::anyhow!("Could not parse issue number from URL"))?;
                (owner, repo, number)
            }
            _ => anyhow::bail!("Could not parse owner/repo from issue URL: {}", issue_url),
        };

        let token = match self.resolve_github_token() {
            Ok(t) => t,
            Err(e) => {
                anyhow::bail!("GitHub token not configured: {}", e);
            }
        };

        let client = github::GithubRestClient::new(&token);

        // Load the agent definition to get the GitHub username for the worker
        // Worker IDs are typically in format "forge-1", "forge-2", etc.
        let agent_type = worker_id.split('-').next().unwrap_or("forge");
        let agent_def_path = self
            .registry_path
            .parent()
            .map(|p| p.join("agents").join(format!("{}.agent.md", agent_type)))
            .unwrap_or_else(|| {
                PathBuf::from(format!(
                    "orchestration/agent/agents/{}.agent.md",
                    agent_type
                ))
            });

        let github_username = match AgentDef::load(&agent_def_path) {
            Ok(def) => def.github.trim().to_string(),
            Err(e) => {
                warn!(
                    worker_id,
                    agent_type,
                    path = %agent_def_path.display(),
                    error = %e,
                    "Failed to load agent definition — using empty GitHub username"
                );
                String::new()
            }
        };

        // Only attempt assignment if a valid GitHub username is configured
        let assignee_display = if !github_username.is_empty() {
            match client
                .assign_issue(owner, repo, issue_number, &github_username)
                .await
            {
                Ok(_) => github_username.clone(),
                Err(e) => {
                    let err_str = e.to_string();
                    // Check if it's a 422 validation error (invalid assignee)
                    if err_str.contains("422") || err_str.contains("Validation Failed") {
                        warn!(
                            worker_id,
                            ticket_id,
                            github_username,
                            error = %e,
                            "GitHub user '{}' is not a valid assignee for this repository — skipping assignment",
                            github_username
                        );
                        // Continue without failing - assignment is not critical
                        github_username.clone()
                    } else {
                        return Err(e);
                    }
                }
            }
        } else {
            warn!(
                worker_id,
                ticket_id, "No GitHub username configured for worker — skipping assignment"
            );
            "FORGE".to_string()
        };

        info!(
            worker_id,
            ticket_id,
            issue_url,
            assignee = assignee_display,
            "Successfully synced assignment to GitHub"
        );

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
        recovery.needs_recovery = recovery.has_unmerged_prs
            || recovery.has_orphaned_tickets
            || recovery.has_stale_workers
            || recovery.has_completed_without_pr;

        recovery
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
        let recovery = Self::reconcile(&tickets, &worker_slots_map, &pending_prs_vec);

        if recovery.needs_recovery {
            info!(
                unmerged_prs = recovery.unmerged_prs.len(),
                orphaned_tickets = recovery.orphaned_tickets.len(),
                stale_workers = recovery.stale_workers.len(),
                completed_without_pr = recovery.completed_without_pr.len(),
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

        let registry = Registry::load(&self.registry_path)?;
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
                    if let Some(slot) = slots.get_mut(worker_id) {
                        slot.status = WorkerStatus::Assigned {
                            ticket_id: ticket_id.clone(),
                            issue_url: decision.issue_url.clone(),
                        };
                        store
                            .set(KEY_WORKER_SLOTS, serde_json::to_value(slots)?)
                            .await;
                        info!(worker_id, ticket_id, issue_url = ?decision.issue_url, "Nexus: Store updated with NEW worker assignment");
                    }

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
