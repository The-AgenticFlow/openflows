//! agent-sentinel — SENTINEL adversarial review node (Coder-only redesign).
//!
//! Thin flow node that reads harness-written review keys from SharedStore
//! and routes based on the sentinel's verdict (approve → vessel, reject → forge).
//! The actual review intelligence lives in the Coder Agent (control plane).

use anyhow::Result;
use async_trait::async_trait;
use coder_client::{ChatStatus, CoderClient};
use config::state::{
    full_ticket_key, full_ticket_key_flat, review_action_key, review_chat_key, review_verdict_key,
    KEY_PENDING_PRS, KEY_TICKETS, KEY_TICKET_CHAT, KEY_TICKET_CHAT_ACTION, KEY_TICKET_STATUS,
    KEY_WORKER_SLOTS, REVIEW_TYPE_PLANNING_GATE, REVIEW_TYPE_PR,
};
use config::{Envconfig, Ticket, TicketStatus, WorkerSlot, WorkerStatus};
use pocketflow_core::{node::PAUSE_SIGNAL, Action, Node, SharedStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use tracing::{debug, info, warn};

const ACTION_REVIEW_APPROVE: &str = "review_approve";
const ACTION_REVIEW_REJECT: &str = "review_reject";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub verdict: String,
    pub report: String,
    pub pr_number: Option<u64>,
}

pub struct SentinelNode {
    #[allow(dead_code)]
    registry_path: std::path::PathBuf,
}

impl SentinelNode {
    pub fn new(registry_path: impl Into<std::path::PathBuf>) -> Self {
        Self {
            registry_path: registry_path.into(),
        }
    }

    async fn coder_client_from_store(store: &SharedStore) -> Option<CoderClient> {
        let coder = config::CoderConfig::init_from_env().ok();
        let coder_url: Option<String> = store
            .get_typed("coder_url")
            .await
            .or_else(|| coder.as_ref().map(|c| c.url.clone()));
        let coder_token: Option<String> = coder
            .and_then(|c| c.session_token)
            .or_else(|| std::env::var("CODER_API_TOKEN").ok());
        let coder_token = if coder_token.as_deref().is_some_and(|t| !t.is_empty()) {
            coder_token
        } else {
            store.get_typed("coder_api_token").await
        };
        match (coder_url, coder_token) {
            (Some(url), Some(token)) if !url.is_empty() && !token.is_empty() => {
                let client = CoderClient::new(&url, &token);
                client.resolve_current_user().await.ok();
                Some(client)
            }
            _ => None,
        }
    }

    async fn send_rejection_follow_up(
        client: &CoderClient,
        chat_id: &str,
        ticket_id: &str,
        report: &str,
    ) -> Result<()> {
        let follow_up = format!(
            "Your review was REJECTED. Please address the following issues and re-submit:\n\n{}",
            report
        );
        client
            .send_chat_message(
                chat_id,
                vec![coder_client::types::ChatInputPart::text(&follow_up)],
            )
            .await?;
        info!(chat_id, ticket_id, "Sent rejection follow-up to forge chat");
        Ok(())
    }

    /// Reduce a worker id (`forge-1`) to its role name (`forge`), used for
    /// looking up chat bindings stored under the role name. Matches
    /// `ForgePairNode::worker_role` / `NexusNode::worker_role`.
    fn worker_role(worker_id: &str) -> &str {
        worker_id
            .rsplit_once('-')
            .map(|(base, _)| base)
            .unwrap_or(worker_id)
    }

    async fn release_sentinel_slots_for_ticket(store: &SharedStore, ticket_id: &str) -> Result<()> {
        let mut slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
        let mut changed = false;

        for slot in slots.values_mut() {
            if Self::worker_role(&slot.id) != "sentinel" {
                continue;
            }
            let assigned_ticket = match &slot.status {
                WorkerStatus::Assigned { ticket_id, .. }
                | WorkerStatus::Working { ticket_id, .. }
                | WorkerStatus::Done { ticket_id, .. }
                | WorkerStatus::Suspended { ticket_id, .. } => Some(ticket_id.as_str()),
                WorkerStatus::Idle => None,
            };
            if assigned_ticket == Some(ticket_id) {
                slot.status = WorkerStatus::Idle;
                changed = true;
            }
        }

        if changed {
            store
                .set(KEY_WORKER_SLOTS, serde_json::to_value(slots)?)
                .await;
        }
        Ok(())
    }

    async fn remove_rejected_pr_from_pending(
        store: &SharedStore,
        ticket_id: &str,
        pr_number: Option<u64>,
    ) {
        let mut pending_prs: Vec<Value> =
            store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
        let before = pending_prs.len();
        pending_prs.retain(|pr| {
            let same_ticket = pr.get("ticket_id").and_then(|v| v.as_str()) == Some(ticket_id);
            let same_pr = pr_number
                .and_then(|n| pr.get("number").and_then(|v| v.as_u64()).map(|m| m == n))
                .unwrap_or(false);
            !(same_ticket || same_pr)
        });

        if pending_prs.len() != before {
            let removed = before - pending_prs.len();
            store.set(KEY_PENDING_PRS, json!(pending_prs)).await;
            info!(
                ticket_id,
                pr_number, removed, "Removed rejected PR from pending_prs so Forge can rework"
            );
        }
    }

    /// Construct a GitHub REST client for SENTINEL's review submission from the
    /// environment (external-auth token, same source VESSEL uses).
    fn github_client_from_env() -> Option<github::GithubRestClient> {
        let token = config::GithubConfig::init_from_env()
            .ok()
            .and_then(|g| g.resolve_token())
            .filter(|t| !t.is_empty())?;
        Some(github::GithubRestClient::new(token))
    }

    /// Parse the store's `"repository"` value (`owner/repo`) into its parts.
    fn parse_repository(repository: Option<&str>) -> (String, String) {
        match repository.and_then(|r| r.split_once('/')) {
            Some((owner, repo)) => (owner.to_string(), repo.to_string()),
            None => (String::new(), String::new()),
        }
    }

    /// Resolve the PR number for a review verdict.
    ///
    /// The SENTINEL chat's `review submit` command may omit `--pr`, which leaves
    /// `ReviewPayload.pr_number == None`. When that happens we fall back to the
    /// PR number Forge recorded when it opened the PR
    /// (`ticket:{id}:pr` -> `{pr_number, branch, title}`). This guarantees the
    /// GitHub review submission (approve / request-changes) always targets the
    /// right PR instead of being silently skipped.
    async fn resolve_pr_number(
        store: &SharedStore,
        ticket_id: &str,
        pr_number: Option<u64>,
    ) -> Option<u64> {
        if pr_number.is_some() {
            return pr_number;
        }
        let pr_key = full_ticket_key_flat(ticket_id, "pr");
        #[derive(serde::Deserialize)]
        struct StoredPr {
            pr_number: u64,
        }
        store
            .get_typed::<StoredPr>(&pr_key)
            .await
            .map(|p| p.pr_number)
    }

    /// Derive inline review comments from a SENTINEL report body. Lines matching
    /// a `path:line — message` (or `path:line message`) shape become GitHub
    /// inline review comments so a REQUEST_CHANGES review carries actionable
    /// file:line guidance for FORGE to address.
    fn parse_report_comments(report: &str) -> Vec<github::ReviewCommentInput> {
        let re = regex::Regex::new(r"(?m)^\s*([^\s:]+):(\d+)[\s:\-–—]*\s*(.*)$").unwrap();
        let mut comments = Vec::new();
        for caps in re.captures_iter(report) {
            let Some(path) = caps.get(1) else { continue };
            let Some(line) = caps.get(2) else { continue };
            let msg = caps.get(3).map(|m| m.as_str().trim()).unwrap_or("").trim();
            let Ok(line_num) = line.as_str().parse::<u64>() else {
                continue;
            };
            if !path.as_str().is_empty() && !msg.is_empty() {
                comments.push(github::ReviewCommentInput {
                    path: path.as_str().to_string(),
                    line: line_num,
                    body: msg.to_string(),
                });
            }
        }
        comments
    }

    /// Submit SENTINEL's verdict as a GitHub PR review. Non-fatal: any failure
    /// (e.g. missing token, insufficient scope, network) is logged and the
    /// sharedstore verdict flow is never blocked by it.
    async fn submit_github_review(
        store: &SharedStore,
        pr_number: u64,
        event: &str,
        body: &str,
        comments: Vec<github::ReviewCommentInput>,
    ) {
        let repository: Option<String> = store.get_typed("repository").await;
        let (owner, repo) = Self::parse_repository(repository.as_deref());
        if owner.is_empty() || repo.is_empty() {
            warn!(
                pr_number,
                event, "Repository info missing — cannot submit GitHub PR review"
            );
            return;
        }
        let Some(client) = Self::github_client_from_env() else {
            warn!(
                pr_number,
                event, "No GitHub token — cannot submit GitHub PR review"
            );
            return;
        };
        if let Err(e) = client
            .submit_pull_request_review(&owner, &repo, pr_number, event, body, comments)
            .await
        {
            warn!(
                pr_number,
                event,
                error = %e,
                "Failed to submit GitHub PR review (non-fatal)"
            );
        } else {
            info!(pr_number, event, "Submitted GitHub PR review");
        }
    }
}

#[async_trait]
impl Node for SentinelNode {
    fn name(&self) -> &str {
        "sentinel"
    }

    async fn prep(&self, store: &SharedStore) -> Result<Value> {
        let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        let _slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let mut reviewable = Vec::new();
        let mut planning_gate_pending = Vec::new();

        for ticket in &tickets {
            let worker_id = match &ticket.status {
                TicketStatus::InProgress { worker_id } => worker_id.clone(),
                TicketStatus::Assigned { worker_id } => worker_id.clone(),
                _ => continue,
            };

            // ── Check for PR review verdicts ──
            let review_key = review_verdict_key(&ticket.id, REVIEW_TYPE_PR);
            let review_payload: Option<ReviewPayload> = store.get_typed(&review_key).await;
            let has_review = review_payload.is_some();

            if let Some(review) = review_payload {
                reviewable.push(json!({
                    "ticket_id": ticket.id,
                    "worker_id": worker_id,
                    "verdict": review.verdict,
                    "report": review.report,
                    "pr_number": review.pr_number,
                    "review_type": "pr_review",
                }));
            }

            // ── Check for planning gate review status ──
            // If SENTINEL has a chat for this ticket in planning phase, check if
            // the gate has been approved. If NOT yet approved, mark it as
            // pending planning review so post() can handle it.
            let status_key = full_ticket_key_flat(&ticket.id, KEY_TICKET_STATUS);
            let status_json: Option<serde_json::Value> = store.get_typed(&status_key).await;
            let phase = status_json
                .as_ref()
                .and_then(|v| v.get("phase"))
                .and_then(|v| v.as_str());

            if phase == Some("planning") {
                // Check if gate already approved
                let gate_key = format!("ticket:{}:gate:planning", ticket.id);
                let gate_approval: Option<serde_json::Value> = store.get_typed(&gate_key).await;

                if gate_approval.is_none() {
                    // Gate not yet approved — SENTINEL is reviewing or needs to review
                    let chat_key = review_chat_key(&ticket.id, REVIEW_TYPE_PLANNING_GATE);
                    let chat_id: Option<String> = store.get_typed(&chat_key).await;

                    // Only add to planning_gate_pending if SENTINEL has been spawned
                    // (chat exists). If no chat yet, NEXUS still needs to spawn it.
                    if chat_id.is_some() {
                        planning_gate_pending.push(json!({
                            "ticket_id": ticket.id,
                            "worker_id": worker_id,
                            "review_type": "planning_gate",
                        }));
                    }
                } else {
                    // Gate is approved — this is handled by ForgePairNode detecting
                    // the phase transition from planning → building
                    debug!(
                        ticket_id = %ticket.id,
                        "Planning gate already approved — SENTINEL review complete"
                    );
                }
            }

            // Monitor the review-type-scoped SENTINEL chat for the current phase so
            // a planning-gate chat and a PR-review chat are tracked independently.
            let (monitor_chat_key, monitor_action_key) = if phase == Some("planning") {
                (
                    review_chat_key(&ticket.id, REVIEW_TYPE_PLANNING_GATE),
                    review_action_key(&ticket.id, REVIEW_TYPE_PLANNING_GATE),
                )
            } else {
                (
                    review_chat_key(&ticket.id, REVIEW_TYPE_PR),
                    review_action_key(&ticket.id, REVIEW_TYPE_PR),
                )
            };
            let chat_id: Option<String> = store.get_typed(&monitor_chat_key).await;
            if let Some(chat_id) = chat_id {
                if let Some(client) = Self::coder_client_from_store(store).await {
                    if let Ok(chat) = client.get_chat(&chat_id).await {
                        let action_key = monitor_action_key;
                        let last_action: Option<String> = store.get_typed(&action_key).await;

                        match chat.status() {
                            ChatStatus::Running => {
                                debug!(
                                    ticket_id = %ticket.id,
                                    "Sentinel chat still running — waiting for review"
                                );
                            }
                            ChatStatus::Waiting
                                if (last_action.as_deref() == Some("completed")
                                    || last_action.is_none())
                                    && !has_review =>
                            {
                                info!(
                                    ticket_id = %ticket.id,
                                    "Sentinel chat waiting but no review written yet — sending follow-up"
                                );
                            }
                            ChatStatus::Error => {
                                warn!(
                                    ticket_id = %ticket.id,
                                    "Sentinel chat in error status"
                                );
                                store.set(&action_key, json!("interrupted")).await;
                            }
                            ChatStatus::RequiresAction => {
                                info!(
                                    ticket_id = %ticket.id,
                                    "Sentinel chat requires_action"
                                );
                            }
                            _ => {}
                        }
                    }
                }
            }
        }

        Ok(json!({
            "reviewable": reviewable,
            "planning_gate_pending": planning_gate_pending,
        }))
    }

    async fn exec(&self, prep_result: Value) -> Result<Value> {
        let reviewable = prep_result["reviewable"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        let planning_gate_pending = prep_result["planning_gate_pending"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        if reviewable.is_empty() && planning_gate_pending.is_empty() {
            return Ok(
                json!({ "verdicts": [], "has_reviews": false, "has_planning_gates": false }),
            );
        }

        info!(
            review_count = reviewable.len(),
            planning_gate_count = planning_gate_pending.len(),
            "Sentinel: processing reviews and planning gates"
        );

        let mut verdicts = Vec::new();
        for review in &reviewable {
            let ticket_id = review["ticket_id"].as_str().unwrap_or("");
            let worker_id = review["worker_id"].as_str().unwrap_or("");
            let verdict = review["verdict"].as_str().unwrap_or("");
            let review_type = review["review_type"].as_str().unwrap_or("pr_review");
            verdicts.push(json!({
                "ticket_id": ticket_id,
                "worker_id": worker_id,
                "verdict": verdict,
                "review_type": review_type,
            }));
        }

        // Planning gate tickets are pending review by SENTINEL (chat is active).
        // The actual review (approve/reject) happens inside the chat — the
        // controller just needs to route these tickets correctly.
        // If a planning gate has been approved by the chat, it will be
        // detected in post() via the gate key in SharedStore.
        for gate in &planning_gate_pending {
            let ticket_id = gate["ticket_id"].as_str().unwrap_or("");
            let worker_id = gate["worker_id"].as_str().unwrap_or("");
            verdicts.push(json!({
                "ticket_id": ticket_id,
                "worker_id": worker_id,
                "verdict": "planning_gate_pending",
                "review_type": "planning_gate",
            }));
        }

        Ok(json!({
            "verdicts": verdicts,
            "has_reviews": !reviewable.is_empty(),
            "has_planning_gates": !planning_gate_pending.is_empty(),
        }))
    }

    async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action> {
        let verdicts: Vec<Value> = exec_result["verdicts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has_reviews = exec_result["has_reviews"].as_bool().unwrap_or(false);
        let has_planning_gates = exec_result["has_planning_gates"].as_bool().unwrap_or(false);

        if !has_reviews && !has_planning_gates {
            debug!("Sentinel: no reviews or planning gates to process");
            return Ok(Action::new("no_work"));
        }

        let mut any_approved = false;
        let mut any_rejected = false;
        let mut any_planning_approved = false;
        let client = Self::coder_client_from_store(store).await;

        for verdict in &verdicts {
            let ticket_id = verdict["ticket_id"].as_str().unwrap_or("");
            let verdict_str = verdict["verdict"].as_str().unwrap_or("");
            let _review_type = verdict["review_type"].as_str().unwrap_or("pr_review");

            match verdict_str {
                "approve" => {
                    info!(ticket_id, "Sentinel: review APPROVED — routing to vessel");

                    let status_key = full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS);
                    store.set(&status_key, json!("approved")).await;

                    let action_key = review_action_key(ticket_id, REVIEW_TYPE_PR);
                    store.set(&action_key, json!("completed")).await;

                    // Read the review payload (pr_number, report) before consuming
                    // it so we can mirror the approve verdict on GitHub. Backfill
                    // the PR number from the ticket's stored PR info when the
                    // verdict omitted it, so the APPROVE review always lands.
                    let review_key = review_verdict_key(ticket_id, REVIEW_TYPE_PR);
                    let review_payload = store.get_typed::<ReviewPayload>(&review_key).await;
                    let pr_number = Self::resolve_pr_number(
                        store,
                        ticket_id,
                        review_payload.as_ref().and_then(|r| r.pr_number),
                    )
                    .await;
                    if let Some(pr_number) = pr_number {
                        let report = review_payload
                            .as_ref()
                            .map(|r| r.report.clone())
                            .unwrap_or_default();
                        Self::submit_github_review(
                            store,
                            pr_number,
                            "APPROVE",
                            if report.trim().is_empty() {
                                "Review approved by SENTINEL."
                            } else {
                                &report
                            },
                            Vec::new(),
                        )
                        .await;
                    }

                    // Consume the review verdict so it is not replayed on the
                    // next poll cycle.
                    store.del(&review_key).await;

                    // Release the SENTINEL reviewer slot, not the Forge owner
                    // carried in `worker_id`.
                    Self::release_sentinel_slots_for_ticket(store, ticket_id).await?;

                    any_approved = true;
                }
                "reject" => {
                    info!(
                        ticket_id,
                        "Sentinel: review REJECTED — routing back to forge"
                    );

                    let review_key = review_verdict_key(ticket_id, REVIEW_TYPE_PR);
                    let review_payload = store.get_typed::<ReviewPayload>(&review_key).await;
                    let report = review_payload
                        .as_ref()
                        .map(|r| r.report.clone())
                        .unwrap_or_default();
                    let pr_number = Self::resolve_pr_number(
                        store,
                        ticket_id,
                        review_payload.as_ref().and_then(|r| r.pr_number),
                    )
                    .await;

                    // Mirror the reject verdict on GitHub: submit a
                    // REQUEST_CHANGES review carrying inline comments derived
                    // from the report's file:line guidance. Non-fatal.
                    if let Some(pr_number) = pr_number {
                        let comments = Self::parse_report_comments(&report);
                        Self::submit_github_review(
                            store,
                            pr_number,
                            "REQUEST_CHANGES",
                            if report.trim().is_empty() {
                                "Changes requested by SENTINEL."
                            } else {
                                &report
                            },
                            comments,
                        )
                        .await;
                    }

                    // Look up the FORGE chat via its role name (e.g. `forge`), not
                    // the literal worker id (`forge-1`). The chat bindings are stored
                    // under the role name as `ticket:{id}:chat:{role}`, so looking up
                    // by worker_id always misses and the rejection follow-up never
                    // lands in FORGE's existing chat.
                    let worker_id = verdict["worker_id"].as_str().unwrap_or("");
                    let role = if worker_id.is_empty() {
                        "forge"
                    } else {
                        Self::worker_role(worker_id)
                    };
                    let forge_chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, role);
                    let forge_chat_id: Option<String> = store.get_typed(&forge_chat_key).await;
                    let forge_action_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, role);

                    if let (Some(ref client), Some(ref chat_id)) = (&client, &forge_chat_id) {
                        if let Err(e) =
                            Self::send_rejection_follow_up(client, chat_id, ticket_id, &report)
                                .await
                        {
                            warn!(
                                ticket_id,
                                error = %e,
                                "Failed to send rejection follow-up to forge"
                            );
                            store.set(&forge_action_key, json!("resume_failed")).await;
                        } else {
                            store.set(&forge_action_key, json!("follow_up_sent")).await;
                        }
                    } else {
                        warn!(
                            ticket_id,
                            worker_id,
                            has_client = client.is_some(),
                            has_chat_id = forge_chat_id.is_some(),
                            "Cannot send rejection follow-up — missing Coder client or forge chat"
                        );
                        store.set(&forge_action_key, json!("resume_needed")).await;
                    }

                    Self::remove_rejected_pr_from_pending(store, ticket_id, pr_number).await;

                    let sentinel_chat_key = review_chat_key(ticket_id, REVIEW_TYPE_PR);
                    if let Some(ref client) = &client {
                        if let Some(sentinel_chat_id) =
                            store.get_typed::<String>(&sentinel_chat_key).await
                        {
                            if let Err(e) = client.archive_chat(&sentinel_chat_id).await {
                                warn!(
                                    ticket_id,
                                    error = %e,
                                    "Failed to archive sentinel chat after rejection"
                                );
                            }
                        }
                    }

                    // Remove the sentinel chat binding; leaving it makes Nexus
                    // treat the archived chat as orphaned and re-spawn a reviewer.
                    store.del(&sentinel_chat_key).await;

                    // Move the ticket out of review_ready so Nexus won't re-spawn
                    // a reviewer until Forge re-arms it via `status set review_ready`.
                    let status_key = full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS);
                    let ts = std::time::SystemTime::now()
                        .duration_since(std::time::UNIX_EPOCH)
                        .unwrap_or_default()
                        .as_secs();
                    let rework_key = full_ticket_key_flat(ticket_id, "rework");
                    store
                        .set(
                            &rework_key,
                            json!({
                                "source": "sentinel",
                                "verdict": "reject",
                                "report": report,
                                "pr_number": pr_number,
                                "phase": "building",
                                "ts": ts,
                            }),
                        )
                        .await;
                    store
                        .set(
                            &status_key,
                            json!({ "phase": "building", "role": "forge", "ts": ts }),
                        )
                        .await;

                    let action_key = review_action_key(ticket_id, REVIEW_TYPE_PR);
                    store.set(&action_key, json!("completed")).await;

                    // Consume the review verdict so it is not replayed on the
                    // next poll cycle.
                    store.del(&review_key).await;

                    // Release only the SENTINEL reviewer slot. The `worker_id`
                    // carried on a PR verdict is the FORGE worker that owns the
                    // ticket; freeing it would make Nexus lose the active rework
                    // session instead of resuming it.
                    Self::release_sentinel_slots_for_ticket(store, ticket_id).await?;

                    any_rejected = true;
                }
                "planning_gate_pending" => {
                    // SENTINEL chat is actively reviewing the plan.
                    // Check if the planning gate has been approved since prep() ran.
                    // The SENTINEL chat runs `openflows-harness gate approve --phase planning`
                    // inside the workspace, which writes to SharedStore.
                    let gate_key = format!("ticket:{}:gate:planning", ticket_id);
                    let gate_approval: Option<serde_json::Value> = store.get_typed(&gate_key).await;

                    if gate_approval.is_some() {
                        // TASK 4: Check if PLAN artifact exists before approving gate.
                        // Per issue #143: "Sentinel gate policy: hard-fail (never approve)
                        // when PLAN.md / pair:{id}:plan is missing or unreadable."
                        let plan_key = format!("pair:{}:plan", ticket_id);
                        let plan_exists: bool = store.get(&plan_key).await.is_some();

                        if !plan_exists {
                            // Plan is missing — refuse to approve gate
                            warn!(
                                ticket_id,
                                "Sentinel: gate BLOCKED — PLAN artifact missing or unreadable \
                                 (issue #143 task 4)"
                            );

                            // Emit blocked verdict via review payload
                            let review_payload = ReviewPayload {
                                verdict: "blocked".to_string(),
                                report: "Gate approval blocked: PLAN.md missing or unreadable. \
                                         Sentinel cannot approve acceptance criteria without the plan."
                                    .to_string(),
                                pr_number: None,
                            };
                            let review_key =
                                review_verdict_key(ticket_id, REVIEW_TYPE_PLANNING_GATE);
                            store
                                .set(&review_key, serde_json::to_value(&review_payload).unwrap())
                                .await;

                            // Archive the sentinel chat
                            let sentinel_chat_key =
                                review_chat_key(ticket_id, REVIEW_TYPE_PLANNING_GATE);
                            if let Some(ref client) = &client {
                                if let Some(sentinel_chat_id) =
                                    store.get_typed::<String>(&sentinel_chat_key).await
                                {
                                    if let Err(e) = client.archive_chat(&sentinel_chat_id).await {
                                        warn!(
                                            ticket_id,
                                            error = %e,
                                            "Failed to archive sentinel chat after gate refusal"
                                        );
                                    }
                                }
                            }

                            // Mark action as completed and continue
                            let action_key =
                                review_action_key(ticket_id, REVIEW_TYPE_PLANNING_GATE);
                            store.set(&action_key, json!("completed")).await;
                        } else {
                            info!(
                                ticket_id,
                                "Sentinel: planning gate APPROVED — FORGE can proceed to building"
                            );

                            // Archive the sentinel chat since gate review is complete
                            let sentinel_chat_key =
                                review_chat_key(ticket_id, REVIEW_TYPE_PLANNING_GATE);
                            if let Some(ref client) = &client {
                                if let Some(sentinel_chat_id) =
                                    store.get_typed::<String>(&sentinel_chat_key).await
                                {
                                    if let Err(e) = client.archive_chat(&sentinel_chat_id).await {
                                        warn!(
                                            ticket_id,
                                            error = %e,
                                            "Failed to archive sentinel chat after planning gate approval"
                                        );
                                    }
                                }
                            }

                            // Release the SENTINEL reviewer slot, not the Forge
                            // owner carried in `worker_id`.
                            Self::release_sentinel_slots_for_ticket(store, ticket_id).await?;

                            // Clear the sentinel chat binding now that the planning-gate
                            // review is complete. The planning and PR reviews are
                            // namespaced by review type
                            // (`ticket:{id}:chat:sentinel:planning_gate` vs
                            // `ticket:{id}:chat:sentinel:pr_review`), so clearing the
                            // planning binding here keeps the namespaces clean and
                            // guarantees a fresh PR-review sentinel is spawned later
                            // (mirrors the reject path, which deletes its binding).
                            store.del(&sentinel_chat_key).await;
                            let sentinel_action_key =
                                review_action_key(ticket_id, REVIEW_TYPE_PLANNING_GATE);
                            store.del(&sentinel_action_key).await;

                            any_planning_approved = true;
                        }
                    } else {
                        info!(
                            ticket_id,
                            "Sentinel: planning gate review in progress — waiting for chat to complete"
                        );
                        // Gate not yet approved — pause and check again on next poll
                    }
                }
                _ => {
                    warn!(
                        ticket_id,
                        verdict = verdict_str,
                        "Sentinel: unknown verdict — skipping"
                    );
                }
            }
        }

        // Priority: PR approved > planning gate approved > PR rejected > no work
        if any_approved {
            Ok(Action::new(ACTION_REVIEW_APPROVE))
        } else if any_planning_approved {
            // Planning gate approved — FORGE can now proceed to building.
            // Route back to nexus so it can detect the gate approval and
            // allow FORGE's workflow to continue.
            info!("Sentinel: planning gate approved — routing back to nexus for FORGE to resume");
            Ok(Action::new("no_work"))
        } else if any_rejected {
            Ok(Action::new(ACTION_REVIEW_REJECT))
        } else {
            // Planning gate reviews are in progress but not yet complete.
            // Pause and let the next poll check again.
            Ok(Action::new(PAUSE_SIGNAL))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn node() -> SentinelNode {
        SentinelNode::new("sentinel.agent.md")
    }

    #[test]
    fn worker_role_strips_numeric_suffix() {
        assert_eq!(SentinelNode::worker_role("forge-1"), "forge");
        assert_eq!(SentinelNode::worker_role("forge-42"), "forge");
        assert_eq!(SentinelNode::worker_role("sentinel"), "sentinel");
        assert_eq!(SentinelNode::worker_role("vessel-1"), "vessel");
    }

    #[tokio::test]
    async fn exec_preserves_worker_id_for_pr_review_and_planning_gate() {
        let prep = json!({
            "reviewable": [{
                "ticket_id": "T-1",
                "worker_id": "forge-1",
                "verdict": "reject",
                "report": "fix it",
                "review_type": "pr_review",
            }],
            "planning_gate_pending": [{
                "ticket_id": "T-2",
                "worker_id": "forge-2",
                "review_type": "planning_gate",
            }],
        });

        let out = node().exec(prep).await.unwrap();
        let verdicts = out["verdicts"].as_array().unwrap();

        let pr = verdicts
            .iter()
            .find(|v| v["review_type"] == "pr_review")
            .unwrap();
        assert_eq!(pr["worker_id"], "forge-1");
        assert_eq!(pr["verdict"], "reject");

        let gate = verdicts
            .iter()
            .find(|v| v["review_type"] == "planning_gate")
            .unwrap();
        assert_eq!(gate["worker_id"], "forge-2");
        assert_eq!(gate["verdict"], "planning_gate_pending");
    }

    #[tokio::test]
    async fn post_reject_resolves_forge_chat_by_role_and_resets_state() {
        let store = SharedStore::new_in_memory();
        let ticket_id = "T-9";

        // Seed the sentinel review payload (reject).
        let review_key = review_verdict_key(ticket_id, REVIEW_TYPE_PR);
        let payload = ReviewPayload {
            verdict: "reject".to_string(),
            report: "src/api.rs:78 — missing pagination; required per spec".to_string(),
            pr_number: Some(42),
        };
        store
            .set(&review_key, serde_json::to_value(&payload).unwrap())
            .await;

        // Seed a sentinel chat binding (archived + removed on reject).
        let sentinel_chat_key = review_chat_key(ticket_id, REVIEW_TYPE_PR);
        store
            .set(&sentinel_chat_key, json!("chat-sentinel-1"))
            .await;

        // Seed the forge chat binding under the ROLE name (`forge`), not the
        // worker id (`forge-1`). The reject path must resolve to this key.
        let forge_chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, "forge");
        store.set(&forge_chat_key, json!("chat-forge-1")).await;

        // Seed busy worker slots so we can observe that rejection releases the
        // reviewer but keeps the Forge owner active for rework.
        let mut slots: HashMap<String, WorkerSlot> = HashMap::new();
        slots.insert(
            "forge-1".to_string(),
            WorkerSlot {
                id: "forge-1".to_string(),
                status: WorkerStatus::Working {
                    ticket_id: ticket_id.to_string(),
                    issue_url: None,
                },
                workspace_id: None,
            },
        );
        slots.insert(
            "sentinel".to_string(),
            WorkerSlot {
                id: "sentinel".to_string(),
                status: WorkerStatus::Assigned {
                    ticket_id: ticket_id.to_string(),
                    issue_url: None,
                },
                workspace_id: None,
            },
        );
        store
            .set(KEY_WORKER_SLOTS, serde_json::to_value(&slots).unwrap())
            .await;
        store
            .set(
                "pending_prs",
                json!([{
                    "number": 42,
                    "ticket_id": ticket_id,
                    "head_branch": "feature/T-9",
                    "worker_id": "forge-1",
                }]),
            )
            .await;

        let exec = json!({
            "verdicts": [{
                "ticket_id": ticket_id,
                "worker_id": "forge-1",
                "verdict": "reject",
                "review_type": "pr_review",
            }],
            "has_reviews": true,
            "has_planning_gates": false,
        });

        let action = node().post(&store, exec).await.unwrap();
        assert_eq!(action.as_str(), ACTION_REVIEW_REJECT);

        // Review verdict consumed.
        let consumed: Option<ReviewPayload> = store.get_typed(&review_key).await;
        assert!(
            consumed.is_none(),
            "review key should be deleted after reject"
        );

        // Sentinel chat binding removed so Nexus won't re-spawn the reviewer.
        let sentinel_chat: Option<String> = store.get_typed(&sentinel_chat_key).await;
        assert!(
            sentinel_chat.is_none(),
            "sentinel chat binding should be deleted"
        );

        // Status reset to building/forge so Nexus won't re-spawn from review_ready.
        let status_key = full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS);
        let status: Option<Value> = store.get_typed(&status_key).await;
        let status = status.expect("status should be written on reject");
        assert_eq!(status["phase"], "building");
        assert_eq!(status["role"], "forge");

        let pending_prs: Vec<Value> = store.get_typed("pending_prs").await.unwrap();
        assert!(
            pending_prs.is_empty(),
            "rejected PR should be removed from pending_prs"
        );

        let rework_key = full_ticket_key_flat(ticket_id, "rework");
        let rework: Option<Value> = store.get_typed(&rework_key).await;
        let rework = rework.expect("rework marker should be written");
        assert_eq!(rework["verdict"], "reject");
        assert_eq!(rework["pr_number"], 42);

        let forge_action_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, "forge");
        let forge_action: Option<String> = store.get_typed(&forge_action_key).await;
        assert_eq!(forge_action.as_deref(), Some("resume_needed"));

        // Sentinel slot is released; Forge remains active for rework.
        let updated: HashMap<String, WorkerSlot> = store.get_typed(KEY_WORKER_SLOTS).await.unwrap();
        let forge_slot = updated.get("forge-1").unwrap();
        assert!(matches!(forge_slot.status, WorkerStatus::Working { .. }));
        let sentinel_slot = updated.get("sentinel").unwrap();
        assert!(matches!(sentinel_slot.status, WorkerStatus::Idle));
    }

    #[test]
    fn action_constants_match_expected_action_names() {
        assert_eq!(ACTION_REVIEW_APPROVE, "review_approve");
        assert_eq!(ACTION_REVIEW_REJECT, "review_reject");
    }

    #[test]
    fn parse_report_comments_extracts_file_line_guidance() {
        let report = "The PR needs work:\n\n\
                      src/api.rs:78 — missing pagination; required per spec\n\
                      src/api.rs:78  also add a cursor param\n\
                      tests/integration.rs:12 — flaky assertion\n\
                      not-a-location line\n\
                      src/models.rs:5 —\n";
        let comments = SentinelNode::parse_report_comments(report);
        assert_eq!(comments.len(), 3);
        assert_eq!(comments[0].path, "src/api.rs");
        assert_eq!(comments[0].line, 78);
        assert!(comments[0].body.contains("missing pagination"));
        assert_eq!(comments[1].path, "src/api.rs");
        assert_eq!(comments[1].line, 78);
        assert!(comments[1].body.contains("cursor param"));
        assert_eq!(comments[2].path, "tests/integration.rs");
        assert_eq!(comments[2].line, 12);
    }

    #[test]
    fn parse_report_comments_empty_for_no_matches() {
        assert!(SentinelNode::parse_report_comments("no guidance here").is_empty());
        assert!(SentinelNode::parse_report_comments("").is_empty());
    }

    #[test]
    fn parse_repository_splits_owner_repo() {
        assert_eq!(
            SentinelNode::parse_repository(Some("owner/repo")),
            ("owner".to_string(), "repo".to_string())
        );
        assert_eq!(
            SentinelNode::parse_repository(Some("single")),
            (String::new(), String::new())
        );
        assert_eq!(
            SentinelNode::parse_repository(None),
            (String::new(), String::new())
        );
    }

    #[tokio::test]
    async fn resolve_pr_number_uses_verdict_when_present() {
        let store = SharedStore::new_in_memory();
        let pr = SentinelNode::resolve_pr_number(&store, "T-1", Some(58)).await;
        assert_eq!(pr, Some(58));
    }

    #[tokio::test]
    async fn resolve_pr_number_backfills_from_ticket_pr_info() {
        let store = SharedStore::new_in_memory();
        let ticket_id = "T-2";
        // Simulate the verdict omitting --pr (pr_number None) while Forge
        // recorded the opened PR at ticket:{id}:pr.
        store
            .set(
                &full_ticket_key_flat(ticket_id, "pr"),
                json!({ "pr_number": 58, "branch": "feature/T-2", "title": "T-2 work" }),
            )
            .await;
        let pr = SentinelNode::resolve_pr_number(&store, ticket_id, None).await;
        assert_eq!(pr, Some(58));
    }

    #[tokio::test]
    async fn resolve_pr_number_none_when_no_verdict_and_no_pr_info() {
        let store = SharedStore::new_in_memory();
        let pr = SentinelNode::resolve_pr_number(&store, "T-3", None).await;
        assert_eq!(pr, None);
    }
}
