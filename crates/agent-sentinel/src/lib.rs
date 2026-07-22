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
use config::{Ticket, TicketStatus, WorkerSlot};
use pocketflow_core::{Action, Node, SharedStore};
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
        let coder_url: Option<String> = store
            .get_typed("coder_url")
            .await
            .or_else(|| std::env::var("CODER_URL").ok());
        let coder_token: Option<String> = store
            .get_typed("coder_api_token")
            .await
            .or_else(|| std::env::var("CODER_SESSION_TOKEN").ok())
            .or_else(|| std::env::var("CODER_API_TOKEN").ok());
        match (coder_url, coder_token) {
            (Some(url), Some(token)) if !url.is_empty() && !token.is_empty() => {
                Some(CoderClient::new(&url, &token))
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

        for ticket in &tickets {
            let worker_id = match &ticket.status {
                TicketStatus::InProgress { worker_id } => worker_id.clone(),
                TicketStatus::Assigned { worker_id } => worker_id.clone(),
                _ => continue,
            };

            let review_key = full_ticket_key(&ticket.id, "review", "sentinel");
            let review_json: Option<String> = store.get_typed(&review_key).await;
            let has_review = review_json.is_some();

            if let Some(review_json) = review_json {
                let review: ReviewPayload = match serde_json::from_str(&review_json) {
                    Ok(r) => r,
                    Err(e) => {
                        warn!(
                            ticket_id = %ticket.id,
                            error = %e,
                            "Failed to parse sentinel review payload"
                        );
                        continue;
                    }
                };

                reviewable.push(json!({
                    "ticket_id": ticket.id,
                    "worker_id": worker_id,
                    "verdict": review.verdict,
                    "report": review.report,
                    "pr_number": review.pr_number,
                }));
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
                            ChatStatus::Waiting => {
                                if (last_action.as_deref() == Some("completed")
                                    || last_action.is_none())
                                    && !has_review
                                {
                                    info!(
                                        ticket_id = %ticket.id,
                                        "Sentinel chat waiting but no review written yet — sending follow-up"
                                    );
                                }
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
        }))
    }

    async fn exec(&self, prep_result: Value) -> Result<Value> {
        let reviewable = prep_result["reviewable"]
            .as_array()
            .cloned()
            .unwrap_or_default();

        if reviewable.is_empty() {
            return Ok(json!({ "verdicts": [], "has_reviews": false }));
        }

        info!(
            count = reviewable.len(),
            "Sentinel: processing review verdicts"
        );

        let mut verdicts = Vec::new();
        for review in &reviewable {
            let ticket_id = review["ticket_id"].as_str().unwrap_or("");
            let verdict = review["verdict"].as_str().unwrap_or("");
            verdicts.push(json!({
                "ticket_id": ticket_id,
                "verdict": verdict,
            }));
        }

        Ok(json!({ "verdicts": verdicts, "has_reviews": !verdicts.is_empty() }))
    }

    async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action> {
        let verdicts: Vec<Value> = exec_result["verdicts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let has_reviews = exec_result["has_reviews"].as_bool().unwrap_or(false);

        if !has_reviews {
            debug!("Sentinel: no reviews to process");
            return Ok(Action::new("no_work"));
        }

        let mut any_approved = false;
        let mut any_rejected = false;
        let client = Self::coder_client_from_store(store).await;

        for verdict in &verdicts {
            let ticket_id = verdict["ticket_id"].as_str().unwrap_or("");
            let verdict_str = verdict["verdict"].as_str().unwrap_or("");

            match verdict_str {
                "approve" => {
                    info!(ticket_id, "Sentinel: review APPROVED — routing to vessel");

                    let status_key = full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS);
                    store.set(&status_key, json!("approved")).await;

                    let action_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, "sentinel");
                    store.set(&action_key, json!("completed")).await;

                    any_approved = true;
                }
                "reject" => {
                    info!(
                        ticket_id,
                        "Sentinel: review REJECTED — routing back to forge"
                    );

                    let review_key = full_ticket_key(ticket_id, "review", "sentinel");
                    let review_json: Option<String> = store.get_typed(&review_key).await;
                    let report = review_json
                        .and_then(|j| serde_json::from_str::<ReviewPayload>(&j).ok())
                        .map(|r| r.report)
                        .unwrap_or_default();

                    let forge_chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, "forge");
                    let forge_chat_id: Option<String> = store.get_typed(&forge_chat_key).await;

                    if let (Some(ref client), Some(chat_id)) = (&client, forge_chat_id) {
                        if let Err(e) =
                            Self::send_rejection_follow_up(client, &chat_id, ticket_id, &report)
                                .await
                        {
                            warn!(
                                ticket_id,
                                error = %e,
                                "Failed to send rejection follow-up to forge"
                            );
                        }
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

                    let action_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, "sentinel");
                    store.set(&action_key, json!("completed")).await;

                    any_rejected = true;
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

        if any_approved {
            Ok(Action::new(ACTION_REVIEW_APPROVE))
        } else if any_rejected {
            Ok(Action::new(ACTION_REVIEW_REJECT))
        } else {
            Ok(Action::new("no_work"))
        }
    }
}
