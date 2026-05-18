// crates/agent-nexus/src/lib.rs
use agent_client::{AgentDecision, AgentPersona, AgentRunner};
use anyhow::Result;
use async_trait::async_trait;
use config::{
    state::{KEY_COMMAND_GATE, KEY_PENDING_PRS, KEY_TICKETS, KEY_WORKER_SLOTS},
    Registry, Ticket, TicketStatus, WorkerSlot, WorkerStatus, ACTION_MERGE_PRS, ACTION_NO_WORK,
};
use nexus_chat::{ChatConfig, HumanChannel, HumanCommand, HumanMessage, MessageType, NexusMessage};
use pocketflow_core::{command_gate::CommandGate, node::STOP_SIGNAL, Action, Node, SharedStore};
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
    human_channel: Option<HumanChannel>,
}

impl NexusNode {
    pub fn new(persona_path: impl Into<PathBuf>, registry_path: impl Into<PathBuf>) -> Self {
        Self {
            persona_path: persona_path.into(),
            registry_path: registry_path.into(),
            human_channel: None,
        }
    }

    pub fn with_chat(
        persona_path: impl Into<PathBuf>,
        registry_path: impl Into<PathBuf>,
        store: SharedStore,
        chat_config: ChatConfig,
    ) -> Self {
        let human_channel = if chat_config.is_configured() {
            Some(HumanChannel::new(store, chat_config))
        } else {
            None
        };
        Self {
            persona_path: persona_path.into(),
            registry_path: registry_path.into(),
            human_channel,
        }
    }

    fn resolve_github_token(&self) -> Result<String> {
        let registry = Registry::load(&self.registry_path)?;
        registry.resolve_github_token("nexus")
    }

    async fn notify_human(&self, msg: NexusMessage) {
        if let Some(ref channel) = self.human_channel {
            if let Err(e) = channel.notify(msg).await {
                warn!("Failed to send human notification: {}", e);
            }
        }
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

    async fn pause_ticket(&self, store: &SharedStore, ticket_id: &str) -> Result<()> {
        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        if let Some(ticket) = tickets.iter_mut().find(|t| t.id == ticket_id) {
            let worker_id = match &ticket.status {
                TicketStatus::InProgress { worker_id } => worker_id.clone(),
                TicketStatus::Assigned { worker_id } => worker_id.clone(),
                _ => return Ok(()),
            };

            ticket.status = TicketStatus::AwaitingHuman {
                worker_id: worker_id.clone(),
                reason: "paused_by_human".to_string(),
                attempts: 0,
            };

            let mut slots: HashMap<String, WorkerSlot> =
                store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
            if let Some(slot) = slots.get_mut(&worker_id) {
                slot.status = WorkerStatus::Suspended {
                    ticket_id: ticket_id.to_string(),
                    reason: "paused_by_human".to_string(),
                    issue_url: ticket.issue_url.clone(),
                };
            }
            store.set(KEY_WORKER_SLOTS, json!(slots)).await;
            store.set(KEY_TICKETS, json!(tickets)).await;
        }
        Ok(())
    }

    async fn resume_ticket(&self, store: &SharedStore, ticket_id: &str) -> Result<()> {
        let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        if let Some(ticket) = tickets.iter_mut().find(|t| t.id == ticket_id) {
            if let TicketStatus::AwaitingHuman { worker_id, .. } = &ticket.status {
                let worker_id = worker_id.clone();
                ticket.status = TicketStatus::Open;
                ticket.attempts = 0;

                let mut slots: HashMap<String, WorkerSlot> =
                    store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                if let Some(slot) = slots.get_mut(&worker_id) {
                    slot.status = WorkerStatus::Idle;
                }
                store.set(KEY_WORKER_SLOTS, json!(slots)).await;
            }
        }
        store.set(KEY_TICKETS, json!(tickets)).await;
        Ok(())
    }

    async fn block_worker(&self, store: &SharedStore, worker_id: &str, reason: &str) -> Result<()> {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        if let Some(slot) = slots.get_mut(worker_id) {
            if let WorkerStatus::Assigned { ticket_id, .. }
            | WorkerStatus::Working { ticket_id, .. } = &slot.status
            {
                let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                if let Some(ticket) = tickets.iter_mut().find(|t| t.id == *ticket_id) {
                    ticket.status = TicketStatus::AwaitingHuman {
                        worker_id: worker_id.to_string(),
                        reason: format!("blocked_by_human: {}", reason),
                        attempts: 0,
                    };
                    store.set(KEY_TICKETS, json!(tickets)).await;
                }
                slot.status = WorkerStatus::Suspended {
                    ticket_id: ticket_id.clone(),
                    reason: format!("blocked_by_human: {}", reason),
                    issue_url: None,
                };
            }
        }
        store.set(KEY_WORKER_SLOTS, json!(slots)).await;
        Ok(())
    }

    async fn reroute_work(
        &self,
        store: &SharedStore,
        from_worker: &str,
        to_worker: &str,
    ) -> Result<()> {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let ticket_id = if let Some(from_slot) = slots.get(from_worker) {
            match &from_slot.status {
                WorkerStatus::Assigned { ticket_id, .. }
                | WorkerStatus::Working { ticket_id, .. } => Some(ticket_id.clone()),
                _ => None,
            }
        } else {
            None
        };

        if let Some(ticket_id) = ticket_id {
            if let Some(from_slot) = slots.get_mut(from_worker) {
                from_slot.status = WorkerStatus::Idle;
            }
            if let Some(to_slot) = slots.get_mut(to_worker) {
                let mut tickets: Vec<Ticket> =
                    store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                if let Some(ticket) = tickets.iter_mut().find(|t| t.id == ticket_id) {
                    ticket.status = TicketStatus::Assigned {
                        worker_id: to_worker.to_string(),
                    };
                    to_slot.status = WorkerStatus::Assigned {
                        ticket_id: ticket_id.clone(),
                        issue_url: ticket.issue_url.clone(),
                    };
                    store.set(KEY_TICKETS, json!(tickets)).await;
                }
            }
        }
        store.set(KEY_WORKER_SLOTS, json!(slots)).await;
        Ok(())
    }

    pub async fn process_human_commands(&self, store: &SharedStore) -> Result<()> {
        if let Some(ref channel) = self.human_channel {
            let messages = channel.pending_messages().await;
            for msg in messages {
                let cmd = self.interpret_message(&msg).await;
                if let Some(cmd) = cmd {
                    match cmd.command {
                        MessageType::PauseWorkflow => {
                            if let Some(ticket_id) = &cmd.ticket_id {
                                self.pause_ticket(store, ticket_id).await?;
                                info!(ticket_id, "Paused by human message");
                                self.notify_human(NexusMessage::status_update(&format!(
                                    "Ticket {} paused",
                                    ticket_id
                                )))
                                .await;
                            }
                        }
                        MessageType::ResumeWorkflow => {
                            if let Some(ticket_id) = &cmd.ticket_id {
                                self.resume_ticket(store, ticket_id).await?;
                                info!(ticket_id, "Resumed by human message");
                                self.notify_human(NexusMessage::status_update(&format!(
                                    "Ticket {} resumed",
                                    ticket_id
                                )))
                                .await;
                            }
                        }
                        MessageType::ApproveCommand => {
                            if let Some(worker_id) = &cmd.worker_id {
                                CommandGate::approve(store, worker_id).await?;
                                info!(worker_id, "Approved by human message");
                                self.notify_human(NexusMessage::status_update(&format!(
                                    "Command approved for {}",
                                    worker_id
                                )))
                                .await;
                            }
                        }
                        MessageType::BlockAgent => {
                            if let Some(worker_id) = &cmd.worker_id {
                                let reason = cmd.payload.as_deref().unwrap_or("blocked_by_human");
                                self.block_worker(store, worker_id, reason).await?;
                                info!(worker_id, "Blocked by human message");
                                self.notify_human(NexusMessage::status_update(&format!(
                                    "Worker {} blocked",
                                    worker_id
                                )))
                                .await;
                            }
                        }
                        MessageType::RerouteAgent => {
                            let from_worker = cmd.worker_id.as_deref();
                            let to_worker = cmd.payload.as_deref();
                            if let (Some(from), Some(to)) = (from_worker, to_worker) {
                                self.reroute_work(store, from, to).await?;
                                info!(from, to, "Rerouted by human message");
                                self.notify_human(NexusMessage::status_update(&format!(
                                    "Work rerouted from {} to {}",
                                    from, to
                                )))
                                .await;
                            }
                        }
                        MessageType::AnswerQuestion => {
                            if let Some(ticket_id) = &cmd.ticket_id {
                                let response_key = format!("human_response:{}", ticket_id);
                                if let Some(answer) = &cmd.payload {
                                    store.set(&response_key, json!(answer)).await;
                                }
                            }
                        }
                        _ => {}
                    }
                } else {
                    info!(user = msg.user_id, text = %msg.text, "Message from human not interpreted as command");
                    // Send a helpful reply so the user knows the message was received but not understood
                    let help_text = format!(
                        "I received your message but couldn't interpret it as a command. \
                        Available commands: `pause T-XXX`, `resume T-XXX`, `approve forge-X`, \
                        `block forge-X [reason]`, `reroute forge-X forge-Y`, `answer T-XXX <text>`. \
                        You can also @mention me or start with my name."
                    );
                    self.notify_human(NexusMessage::status_update(&help_text)).await;
                }
                channel.ack_message(&msg).await;
            }
        }
        Ok(())
    }

    async fn interpret_message(&self, msg: &HumanMessage) -> Option<HumanCommand> {
        let text = msg.text.trim();
        let lower = text.to_lowercase();

        // Try pattern-based interpretation first
        if let Some(cmd) = self.try_pattern_match(text, &lower, msg) {
            return Some(cmd);
        }

        // If no pattern matches, use LLM to interpret
        if let Some(ref channel) = self.human_channel {
            if channel.is_dev_mode() {
                return None;
            }
        }

        self.interpret_with_llm(text, msg).await
    }

    fn try_pattern_match(&self, text: &str, lower: &str, msg: &HumanMessage) -> Option<HumanCommand> {
        // Helper: check that lower starts with an exact command word (space or EOL after it)
        let starts_with_cmd = |cmd: &str| lower == cmd || lower.starts_with(&format!("{} ", cmd));
        // Helper: fuzzy match — first word starts with cmd (catches typos like "assigne", "assigning")
        let fuzzy_cmd = |cmd: &str| {
            let first = lower.split_whitespace().next().unwrap_or("");
            first == cmd || first.starts_with(cmd)
        };

        // Direct command patterns
        if starts_with_cmd("pause") || fuzzy_cmd("pause") {
            let ticket_id = extract_ticket_id(text);
            return ticket_id.map(|id| HumanCommand::pause_workflow(&id, &msg.user_id, &msg.channel_id));
        }

        if starts_with_cmd("resume") || fuzzy_cmd("resume") {
            let ticket_id = extract_ticket_id(text);
            return ticket_id.map(|id| HumanCommand::resume_workflow(&id, &msg.user_id, &msg.channel_id));
        }

        if starts_with_cmd("approve") || fuzzy_cmd("approve") {
            let worker_id = extract_worker_id(text);
            return Some(HumanCommand::approve_command(
                &worker_id.unwrap_or_else(|| "unknown".to_string()),
                &msg.user_id,
                &msg.channel_id,
            ));
        }

        if starts_with_cmd("reject") || starts_with_cmd("deny") || fuzzy_cmd("reject") || fuzzy_cmd("deny") {
            let worker_id = extract_worker_id(text);
            return Some(HumanCommand {
                command: MessageType::BlockAgent,
                ticket_id: None,
                worker_id,
                payload: Some("rejected by human".to_string()),
                user_id: msg.user_id.clone(),
                channel_id: msg.channel_id.clone(),
                thread_ts: None,
                timestamp: chrono::Utc::now(),
            });
        }

        if starts_with_cmd("block") || fuzzy_cmd("block") {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() >= 2 {
                let worker_id = parts[1].to_string();
                let reason = parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                return Some(HumanCommand::block_agent(&worker_id, &reason, &msg.user_id, &msg.channel_id));
            }
        }

        if starts_with_cmd("reroute") || starts_with_cmd("reassign") || starts_with_cmd("assign")
            || fuzzy_cmd("reroute") || fuzzy_cmd("reassign") || fuzzy_cmd("assign")
        {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() >= 3 {
                // Try to read two worker IDs (reroute case): "reroute forge-1 forge-2"
                let (from_worker, to_worker) = extract_two_workers(&parts);
                if from_worker.contains('-') && to_worker.contains('-') {
                    return Some(HumanCommand::reroute_agent(&from_worker, &to_worker, &msg.user_id, &msg.channel_id));
                }

                // Single worker assignment case: "assign forge2 to a ticket" or "assign forge-1 to T-001"
                // Try to extract one worker ID and an optional ticket ID
                if parts.len() >= 2 {
                    let worker_id = normalize_worker_id(parts[1]);
                    if worker_id.starts_with("forge-") || worker_id.starts_with("agent-") {
                        let ticket_id = extract_ticket_id(text);
                        // For single worker assignment, we'll use approve_command as the command type
                        // since it's the closest match - approving a worker to work on something
                        return Some(HumanCommand {
                            command: MessageType::ApproveCommand,
                            ticket_id,
                            worker_id: Some(worker_id),
                            payload: Some("assigned by human".to_string()),
                            user_id: msg.user_id.clone(),
                            channel_id: msg.channel_id.clone(),
                            thread_ts: None,
                            timestamp: chrono::Utc::now(),
                        });
                    }
                }
            }
        }

        if starts_with_cmd("answer") || fuzzy_cmd("answer") {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() >= 2 {
                let ticket_id = extract_ticket_id(text).unwrap_or_else(|| parts[1].trim_end_matches(':').to_string());
                let answer = parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                return Some(HumanCommand::answer_question(&ticket_id, &answer, &msg.user_id, &msg.channel_id));
            }
        }

        // Question/answer format: "yes", "no", "option 1", etc.
        if lower == "yes" || lower == "no" || lower.starts_with("option ") {
            // This is likely an answer to a question
            // Try to find the most recent question context
            return Some(HumanCommand {
                command: MessageType::AnswerQuestion,
                ticket_id: None,
                worker_id: None,
                payload: Some(text.to_string()),
                user_id: msg.user_id.clone(),
                channel_id: msg.channel_id.clone(),
                thread_ts: None,
                timestamp: chrono::Utc::now(),
            });
        }

        None
    }

    async fn interpret_with_llm(&self, text: &str, msg: &HumanMessage) -> Option<HumanCommand> {
        // Build a prompt asking the LLM to interpret the message
        // Output must match AgentDecision schema: {action, notes, assign_to, ticket_id, issue_url}
        let prompt = format!(
            r#"You are NEXUS, interpreting a message from a human operator.
Parse the following message into a structured command.

Human message: "{}"

Available action types and required fields:
- pause_workflow: set ticket_id (string, e.g. "T-001")
- resume_workflow: set ticket_id (string)
- approve_command: set assign_to to worker_id (string, e.g. "forge-1")
- block_agent: set assign_to to worker_id (string), put reason in notes (string)
- reroute_agent: set assign_to to from_worker (string, e.g. "forge-1"), put to_worker in notes (string, e.g. "forge-2")
- answer_question: set ticket_id (string), put answer in notes (string)
- status_query: no fields needed
- general_message: no fields needed (use when message is not a command)

RULES:
1. ALL field values must be simple strings or null. NEVER use nested JSON objects.
2. For reroute_agent, put the source worker in "assign_to" and destination worker in "notes".
3. If a worker is mentioned as "forge 1", normalize it to "forge-1".
4. If the message is vague or missing required fields, use general_message.
5. Put any additional context or parameters in the "notes" field.

Respond with ONLY a JSON object matching this schema. No markdown, no explanations.

Example valid responses:
{{"action": "pause_workflow", "notes": "", "assign_to": null, "ticket_id": "T-001", "issue_url": null}}
{{"action": "reroute_agent", "notes": "forge-2", "assign_to": "forge-1", "ticket_id": null, "issue_url": null}}
{{"action": "block_agent", "notes": "stuck on test failure", "assign_to": "forge-1", "ticket_id": null, "issue_url": null}}
{{"action": "general_message", "notes": "", "assign_to": null, "ticket_id": null, "issue_url": null}}"#,
            text
        );

        // Use the existing LLM runner to interpret
        match self.run_llm_interpretation(&prompt).await {
            Ok(result) => {
                if let Some(cmd_type) = result.get("command_type").and_then(|v| v.as_str()) {
                    match cmd_type {
                        "pause_workflow" => {
                            if let Some(ticket_id) = result.get("ticket_id").and_then(|v| v.as_str()) {
                                return Some(HumanCommand::pause_workflow(ticket_id, &msg.user_id, &msg.channel_id));
                            }
                        }
                        "resume_workflow" => {
                            if let Some(ticket_id) = result.get("ticket_id").and_then(|v| v.as_str()) {
                                return Some(HumanCommand::resume_workflow(ticket_id, &msg.user_id, &msg.channel_id));
                            }
                        }
                        "approve_command" => {
                            if let Some(worker_id) = result.get("worker_id").and_then(|v| v.as_str()) {
                                return Some(HumanCommand::approve_command(worker_id, &msg.user_id, &msg.channel_id));
                            }
                        }
                        "block_agent" => {
                            if let Some(worker_id) = result.get("worker_id").and_then(|v| v.as_str()) {
                                let reason = result.get("payload").and_then(|v| v.as_str()).unwrap_or("blocked_by_human").to_string();
                                return Some(HumanCommand::block_agent(worker_id, &reason, &msg.user_id, &msg.channel_id));
                            }
                        }
                        "reroute_agent" => {
                            if let (Some(from), Some(to)) = (
                                result.get("worker_id").and_then(|v| v.as_str()),
                                result.get("payload").and_then(|v| v.as_str()),
                            ) {
                                return Some(HumanCommand::reroute_agent(from, to, &msg.user_id, &msg.channel_id));
                            }
                        }
                        "answer_question" => {
                            if let (Some(ticket_id), Some(answer)) = (
                                result.get("ticket_id").and_then(|v| v.as_str()),
                                result.get("payload").and_then(|v| v.as_str()),
                            ) {
                                return Some(HumanCommand::answer_question(ticket_id, answer, &msg.user_id, &msg.channel_id));
                            }
                        }
                        _ => {}
                    }
                }
                None
            }
            Err(e) => {
                warn!("LLM interpretation failed: {}", e);
                None
            }
        }
    }

    async fn run_llm_interpretation(&self, prompt: &str) -> Result<Value> {
        let registry = Registry::load(&self.registry_path)?;
        let model_backend = registry.get("nexus").and_then(|e| e.model_backend.clone());
        let github_token = self.resolve_github_token()?;

        let mut runner =
            AgentRunner::from_env_with_token(model_backend.as_deref(), &github_token).await?;
        let persona = AgentPersona {
            id: "nexus-interpreter".to_string(),
            role: "interpreter".to_string(),
            system_prompt: "You are a command parser. Respond ONLY with valid JSON.".to_string(),
        };

        let context = json!({
            "prompt": prompt,
            "format": "json"
        });

        let decision: AgentDecision = runner.run(&persona, context, 5).await?;

        // decision.action contains the command type (e.g. "pause_workflow")
        // decision.notes contains additional context or parameters
        // decision.assign_to contains worker_id when applicable
        // decision.ticket_id contains ticket_id when applicable
        let mut result = serde_json::Map::new();
        result.insert("action".to_string(), Value::String(decision.action.clone()));
        result.insert("notes".to_string(), Value::String(decision.notes.clone()));
        result.insert(
            "assign_to".to_string(),
            decision.assign_to.map(Value::String).unwrap_or(Value::Null),
        );
        result.insert(
            "ticket_id".to_string(),
            decision.ticket_id.map(Value::String).unwrap_or(Value::Null),
        );
        result.insert(
            "issue_url".to_string(),
            decision.issue_url.map(Value::String).unwrap_or(Value::Null),
        );

        Ok(Value::Object(result))
    }
}

fn extract_ticket_id(text: &str) -> Option<String> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    for part in parts {
        let cleaned = part.trim_end_matches(':');
        let lower = cleaned.to_lowercase();
        if lower.starts_with("t-") {
            let num = lower.trim_start_matches("t-");
            return Some(format!("T-{}", num.to_uppercase()));
        }
        if lower.starts_with("ticket-") {
            let num = lower.trim_start_matches("ticket-");
            return Some(format!("T-{}", num.to_uppercase()));
        }
    }
    None
}

fn extract_worker_id(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    for part in parts.iter().skip(1) {
        if !part.starts_with("t-") && !part.starts_with("ticket-") {
            return Some(normalize_worker_id(part));
        }
    }
    None
}

/// Normalize informal worker IDs like "forge1" → "forge-1".
fn normalize_worker_id(raw: &str) -> String {
    let lower = raw.to_lowercase();
    // Insert a hyphen between letters and trailing digits (e.g. forge1 -> forge-1)
    let mut result = String::new();
    let mut prev_was_digit = false;
    let mut prev_was_letter = false;
    for ch in lower.chars() {
        let is_digit = ch.is_ascii_digit();
        let is_letter = ch.is_ascii_alphabetic();
        if is_digit && prev_was_letter && !prev_was_digit {
            result.push('-');
        }
        result.push(ch);
        prev_was_digit = is_digit;
        prev_was_letter = is_letter;
    }
    result
}

/// Extract two worker IDs from a word slice, handling "forge 1" → "forge-1" and skipping filler words.
fn extract_two_workers(parts: &[&str]) -> (String, String) {
    if parts.len() < 3 {
        return (String::new(), String::new());
    }
    let (from_worker, next_idx) = read_worker(parts, 1);
    let (to_worker, _) = read_worker(parts, next_idx);
    (from_worker, to_worker)
}

fn read_worker(parts: &[&str], start: usize) -> (String, usize) {
    if start >= parts.len() {
        return (String::new(), start);
    }
    let token = parts[start].to_lowercase();
    // Skip filler words
    if token == "to" || token == "into" || token == "from" {
        return read_worker(parts, start + 1);
    }
    let normalized = normalize_worker_id(parts[start]);
    if normalized.contains('-') {
        return (normalized, start + 1);
    }
    // Check if next part is a digit that should be joined (e.g. "forge 1")
    if start + 1 < parts.len() && parts[start + 1].parse::<u64>().is_ok() {
        let joined = format!("{}{}", parts[start], parts[start + 1]);
        return (normalize_worker_id(&joined), start + 2);
    }
    (normalized, start + 1)
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

        if let Err(e) = self.process_human_commands(store).await {
            warn!("Failed to process human commands: {}", e);
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

                    let workflow_msg = NexusMessage::workflow_started(
                        ticket_id,
                        worker_id,
                        &decision.notes,
                        decision.issue_url.as_deref(),
                    );
                    let assign_msg = NexusMessage::agent_assigned(
                        worker_id,
                        ticket_id,
                        &format!("{} has been assigned to work on {}", worker_id, ticket_id),
                    );

                    let wf_ok = if let Some(ref channel) = self.human_channel {
                        match channel.notify(workflow_msg).await {
                            Ok(()) => true,
                            Err(e) => {
                                warn!(error = %e, "Failed to send workflow notification");
                                false
                            }
                        }
                    } else {
                        false
                    };
                    let assign_ok = if let Some(ref channel) = self.human_channel {
                        match channel.notify(assign_msg).await {
                            Ok(()) => true,
                            Err(e) => {
                                warn!(error = %e, "Failed to send assignment notification");
                                false
                            }
                        }
                    } else {
                        false
                    };

                    info!(
                        worker_id,
                        ticket_id,
                        human_channel = ?self.human_channel.is_some(),
                        workflow_notification_sent = wf_ok,
                        assignment_notification_sent = assign_ok,
                        "📢 NEXUS → HUMAN: {} assigned to {} (workflow_sent={}, assignment_sent={})",
                        ticket_id,
                        worker_id,
                        wf_ok,
                        assign_ok
                    );

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

                self.notify_human(NexusMessage::status_update(&format!(
                    "Command {} for {}",
                    if decision.action == "approve_command" {
                        "approved"
                    } else {
                        "rejected"
                    },
                    worker_id
                )))
                .await;

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
