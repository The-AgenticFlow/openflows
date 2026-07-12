//! agent-forge — FORGE builder node (Coder-only redesign).
//!
//! Thin flow node that monitors Coder Agent Chats for forge workers.
//! Reads harness-written SharedStore keys for routing decisions.
//! The actual coding intelligence lives in the Coder Agent (control plane).

use anyhow::Result;
use async_trait::async_trait;
use coder_client::{ChatStatus, CoderClient};
use config::{
    state::{
        full_ticket_key, full_ticket_key_flat,
        KEY_PENDING_PRS, KEY_TICKET_CHAT, KEY_TICKET_CHAT_ACTION,
        KEY_TICKET_STATUS, KEY_TICKETS,
        KEY_WORKER_SLOTS,
    },
    Ticket, TicketStatus, WorkerSlot, ACTION_EMPTY, ACTION_FAILED, ACTION_PR_OPENED,
};
use pocketflow_core::{Action, BatchNode, SharedStore};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

pub struct ForgePairNode {
    #[allow(dead_code)]
    workspace_root: PathBuf,
    #[allow(dead_code)]
    registry_path: PathBuf,
}

impl ForgePairNode {
    pub fn new(_workspace_root: impl Into<PathBuf>, _github_token: impl Into<String>) -> Self {
        Self {
            workspace_root: _workspace_root.into(),
            registry_path: PathBuf::new(),
        }
    }

    pub fn new_with_registry(
        workspace_root: impl Into<PathBuf>,
        registry_path: impl Into<PathBuf>,
    ) -> Self {
        Self {
            workspace_root: workspace_root.into(),
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

    fn worker_role(worker_id: &str) -> &str {
        worker_id
            .rsplit_once('-')
            .map(|(base, _)| base)
            .unwrap_or(worker_id)
    }

    async fn sync_chat_status_to_store(
        store: &SharedStore,
        ticket_id: &str,
        role: &str,
        chat_status: ChatStatus,
    ) {
        let action_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, role);
        let last_action: Option<String> = store.get_typed(&action_key).await;

        match chat_status {
            ChatStatus::Running => {
                let status_key = full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS);
                let current: Option<String> = store.get_typed(&status_key).await;
                if current.as_deref() != Some("building") && current.as_deref() != Some("planning") {
                    store.set(&status_key, json!("building")).await;
                }
            }
            ChatStatus::Waiting => {
                match last_action.as_deref() {
                    Some("completed") | None => {
                        info!(
                            ticket_id,
                            role,
                            "Chat waiting with chat_action=completed|null — forge work done"
                        );
                    }
                    Some("interrupted") => {
                        info!(
                            ticket_id,
                            role,
                            "Chat waiting after interruption — needs recovery"
                        );
                    }
                    Some("created") | Some("follow_up_sent") => {
                        info!(
                            ticket_id,
                            role,
                            ?last_action,
                            "Chat waiting after initial prompt — agent may need follow-up"
                        );
                    }
                    _ => {}
                }
            }
            ChatStatus::Error => {
                warn!(ticket_id, role, "Forge chat entered error status");
                store.set(&action_key, json!("interrupted")).await;
            }
            ChatStatus::RequiresAction => {
                info!(ticket_id, role, "Forge chat requires_action — setting awaiting_human");
            }
            ChatStatus::Pending => {
                debug!(ticket_id, role, "Forge chat pending");
            }
        }
    }
}

#[async_trait]
impl BatchNode for ForgePairNode {
    fn name(&self) -> &str {
        "forge_pair"
    }

    async fn prep_batch(&self, store: &SharedStore) -> Result<Vec<Value>> {
        let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        let slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        let forge_items: Vec<Value> = tickets
            .iter()
            .filter_map(|ticket| {
                let worker_id = match &ticket.status {
                    TicketStatus::Assigned { worker_id }
                    | TicketStatus::InProgress { worker_id } => worker_id.clone(),
                    _ => return None,
                };

                let role = Self::worker_role(&worker_id);
                if role != "forge" {
                    return None;
                }

                let workspace_id = slots
                    .get(&worker_id)
                    .and_then(|s| s.workspace_id.clone());

                Some(json!({
                    "ticket_id": ticket.id,
                    "worker_id": worker_id,
                    "workspace_id": workspace_id,
                    "status": format!("{:?}", ticket.status),
                }))
            })
            .collect();

        Ok(forge_items)
    }

    async fn exec_one(&self, item: Value) -> Result<Value> {
        let ticket_id = item["ticket_id"].as_str().unwrap_or("");
        let worker_id = item["worker_id"].as_str().unwrap_or("");
        info!(ticket_id, worker_id, "ForgePairNode monitoring forge worker");
        Ok(item)
    }

    async fn post_batch(
        &self,
        store: &SharedStore,
        results: Vec<Result<Value>>,
    ) -> Result<Action> {
        if results.is_empty() {
            return Ok(Action::new(Action::NO_TICKETS));
        }

        let client = Self::coder_client_from_store(store).await;
        let mut has_pr_opened = false;
        let mut has_failed = false;
        let mut has_in_progress = false;

        let tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
        let _slots: HashMap<String, WorkerSlot> =
            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();

        for result in &results {
            let item = match result {
                Ok(v) => v,
                Err(_) => continue,
            };

            let ticket_id = item["ticket_id"].as_str().unwrap_or("");
            let _worker_id = item["worker_id"].as_str().unwrap_or("");
            let role = "forge";

            let chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, role);
            let chat_id: Option<String> = store.get_typed(&chat_key).await;

            if let (Some(ref client), Some(chat_id)) = (&client, chat_id) {
                match client.get_chat(&chat_id).await {
                    Ok(chat) => {
                        Self::sync_chat_status_to_store(
                            store,
                            ticket_id,
                            role,
                            chat.status(),
                        )
                        .await;
                    }
                    Err(e) => {
                        warn!(
                            chat_id = %chat_id,
                            ticket_id,
                            error = %e,
                            "Failed to get forge chat status"
                        );
                    }
                }
            }

            let pending_prs: Vec<Value> =
                store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();
            let ticket_has_pr = pending_prs
                .iter()
                .any(|p| p.get("ticket_id").and_then(|v| v.as_str()) == Some(ticket_id));

            if ticket_has_pr {
                has_pr_opened = true;

                if let Some(ticket) = tickets.iter().find(|t| t.id == ticket_id) {
                    if matches!(ticket.status, TicketStatus::InProgress { .. }) {
                        info!(
                            ticket_id,
                            "Forge completed: PR opened, updating ticket status"
                        );
                    }
                }
            } else {
                let handoff_key = full_ticket_key_flat(ticket_id, "handoff");
                let has_handoff: Option<Value> = store.get_typed(&handoff_key).await;

                if has_handoff.is_some() {
                    info!(
                        ticket_id,
                        "Forge completed: handoff written, PR pending or review-ready"
                    );
                }

                let ticket = tickets.iter().find(|t| t.id == ticket_id);
                match ticket.map(|t| &t.status) {
                    Some(TicketStatus::Failed { .. }) => {
                        has_failed = true;
                    }
                    Some(TicketStatus::AwaitingHuman { .. }) => {
                        has_failed = true;
                    }
                    _ => {
                        has_in_progress = true;
                    }
                }
            }
        }

        info!(
            monitored = results.len(),
            has_pr_opened,
            has_failed,
            has_in_progress,
            "ForgePairNode post_batch summary"
        );

        if has_pr_opened {
            info!("Forge: PR(s) opened — routing to vessel");
            Ok(Action::new(ACTION_PR_OPENED))
        } else if has_failed {
            info!("Forge: failure detected — routing back to nexus");
            Ok(Action::new(ACTION_FAILED))
        } else if has_in_progress {
            info!("Forge: work still in progress — returning empty to cycle");
            Ok(Action::new(ACTION_EMPTY))
        } else {
            Ok(Action::new(ACTION_EMPTY))
        }
    }
}

/// Legacy ForgeNode — kept for backward compatibility. Delegates to ForgePairNode logic.
pub struct ForgeNode {
    inner: ForgePairNode,
}

impl ForgeNode {
    pub fn new(
        workspace_root: impl Into<PathBuf>,
        _persona_path: impl Into<PathBuf>,
        _github_token: impl Into<String>,
    ) -> Self {
        Self {
            inner: ForgePairNode::new(workspace_root, ""),
        }
    }
}

#[async_trait]
impl BatchNode for ForgeNode {
    fn name(&self) -> &str {
        "forge"
    }

    async fn prep_batch(&self, store: &SharedStore) -> Result<Vec<Value>> {
        self.inner.prep_batch(store).await
    }

    async fn exec_one(&self, item: Value) -> Result<Value> {
        self.inner.exec_one(item).await
    }

    async fn post_batch(
        &self,
        store: &SharedStore,
        results: Vec<Result<Value>>,
    ) -> Result<Action> {
        self.inner.post_batch(store, results).await
    }
}
