//! agent-sentinel — SENTINEL adversarial review node (Coder-only redesign).
//!
//! Thin flow node that reads harness-written review keys from SharedStore
//! and routes based on the sentinel's verdict (approve → vessel, reject → forge).
//! The actual review intelligence lives in the Coder Agent (control plane).

use anyhow::Result;
use async_trait::async_trait;
use coder_client::{ChatStatus, CoderClient};
use config::state::{
    full_ticket_key, full_ticket_key_flat, KEY_TICKETS, KEY_TICKET_CHAT, KEY_TICKET_CHAT_ACTION,
    KEY_TICKET_STATUS, KEY_WORKER_SLOTS,
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
            let review_key = full_ticket_key(&ticket.id, "review", "sentinel");
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
                    let chat_key = full_ticket_key(&ticket.id, KEY_TICKET_CHAT, "sentinel");
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

            let chat_key = full_ticket_key(&ticket.id, KEY_TICKET_CHAT, "sentinel");
            let chat_id: Option<String> = store.get_typed(&chat_key).await;
            if let Some(chat_id) = chat_id {
                if let Some(client) = Self::coder_client_from_store(store).await {
                    if let Ok(chat) = client.get_chat(&chat_id).await {
                        let action_key =
                            full_ticket_key(&ticket.id, KEY_TICKET_CHAT_ACTION, "sentinel");
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
            let verdict = review["verdict"].as_str().unwrap_or("");
            let review_type = review["review_type"].as_str().unwrap_or("pr_review");
            verdicts.push(json!({
                "ticket_id": ticket_id,
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
            verdicts.push(json!({
                "ticket_id": ticket_id,
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

                    let action_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, "sentinel");
                    store.set(&action_key, json!("completed")).await;

                    // Consume the review verdict so it is not replayed on the
                    // next poll cycle.
                    let review_key = full_ticket_key(ticket_id, "review", "sentinel");
                    store.del(&review_key).await;

                    // Release the sentinel slot so it's available for future
                    // reviews; otherwise Nexus sees an active sentinel worker and
                    // keeps routing nexus→sentinel, stalling Forge/Vessel.
                    let worker_id = verdict["worker_id"].as_str().unwrap_or("");
                    if !worker_id.is_empty() {
                        let mut slots: HashMap<String, WorkerSlot> =
                            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                        if let Some(slot) = slots.get_mut(worker_id) {
                            slot.status = WorkerStatus::Idle;
                        }
                        store
                            .set(KEY_WORKER_SLOTS, serde_json::to_value(slots)?)
                            .await;
                    }

                    any_approved = true;
                }
                "reject" => {
                    info!(
                        ticket_id,
                        "Sentinel: review REJECTED — routing back to forge"
                    );

                    let review_key = full_ticket_key(ticket_id, "review", "sentinel");
                    let report = store
                        .get_typed::<ReviewPayload>(&review_key)
                        .await
                        .map(|r| r.report)
                        .unwrap_or_default();

                    // Look up the FORGE chat via the assigned worker_id, not the
                    // literal role string "forge". The chat key is stored as
                    // `ticket:{id}:chat:{worker_id}` (e.g. `forge-1`).
                    let worker_id = verdict["worker_id"].as_str().unwrap_or("");
                    let forge_chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, worker_id);
                    let forge_chat_id: Option<String> = store.get_typed(&forge_chat_key).await;

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
                        }
                    } else {
                        warn!(
                            ticket_id,
                            worker_id,
                            has_client = client.is_some(),
                            has_chat_id = forge_chat_id.is_some(),
                            "Cannot send rejection follow-up — missing Coder client or forge chat"
                        );
                    }

                    let sentinel_chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, "sentinel");
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
                    store
                        .set(&status_key, json!({ "phase": "building", "role": "forge", "ts": ts }))
                        .await;

                    let action_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, "sentinel");
                    store.set(&action_key, json!("completed")).await;

                    // Consume the review verdict so it is not replayed on the
                    // next poll cycle.
                    store.del(&review_key).await;

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
                            let review_key = full_ticket_key(ticket_id, "review", "sentinel");
                            store
                                .set(&review_key, serde_json::to_value(&review_payload).unwrap())
                                .await;

                            // Archive the sentinel chat
                            let sentinel_chat_key =
                                full_ticket_key(ticket_id, KEY_TICKET_CHAT, "sentinel");
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
                                full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, "sentinel");
                            store.set(&action_key, json!("completed")).await;
                        } else {
                            info!(
                                ticket_id,
                                "Sentinel: planning gate APPROVED — FORGE can proceed to building"
                            );

                            // Archive the sentinel chat since gate review is complete
                            let sentinel_chat_key =
                                full_ticket_key(ticket_id, KEY_TICKET_CHAT, "sentinel");
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

                            // Release the sentinel slot so it's available for future reviews
                            let worker_id = verdict["worker_id"].as_str().unwrap_or("");
                            if !worker_id.is_empty() {
                                let mut slots: HashMap<String, WorkerSlot> =
                                    store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                                if let Some(slot) = slots.get_mut(worker_id) {
                                    slot.status = WorkerStatus::Idle;
                                }
                                store
                                    .set(KEY_WORKER_SLOTS, serde_json::to_value(slots)?)
                                    .await;
                            }

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
