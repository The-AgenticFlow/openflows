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
        address_review_dispatched_key, address_review_rearmed_key, full_ticket_key,
        full_ticket_key_flat, KEY_PENDING_PRS, KEY_TICKETS, KEY_TICKET_CHAT,
        KEY_TICKET_CHAT_ACTION, KEY_TICKET_STATUS, KEY_WORKER_SLOTS,
    },
    Envconfig, Ticket, TicketStatus, WorkerSlot, ACTION_FAILED, ACTION_PR_OPENED,
};
use pocketflow_core::{node::PAUSE_SIGNAL, Action, BatchNode, SharedStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::path::PathBuf;
use tracing::{debug, info, warn};

/// PR info written by the harness when forge opens a PR.
/// Matches the schema in openflows-harness/src/store.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessPrInfo {
    pub pr_number: u64,
    pub branch: String,
    pub title: String,
}

/// Status payload written by the harness.
/// Matches the schema in openflows-harness/src/store.rs
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HarnessStatus {
    pub phase: String,
    pub role: String,
    pub ts: u64,
}

/// Action to signal that work is ready for review (Sentinel should be spawned)
pub const ACTION_REVIEW_READY: &str = "review_ready";

/// Action to signal that FORGE is in the planning phase and waiting for
/// SENTINEL gate approval. NEXUS picks this up and spawns a SENTINEL chat
/// to review the plan and run `openflows-harness gate approve --phase planning`.
pub const ACTION_PLANNING_GATE: &str = "planning_gate";

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
                debug!(
                    ticket_id,
                    role, "Forge chat running — preserving harness phase"
                );
            }
            ChatStatus::Waiting => match last_action.as_deref() {
                Some("completed") | None => {
                    info!(
                        ticket_id,
                        role, "Chat waiting with chat_action=completed|null — forge work done"
                    );
                }
                Some("interrupted") | Some("resume_needed") | Some("resume_failed") => {
                    info!(
                        ticket_id,
                        role, "Chat waiting after interruption/error — needs same-session resume"
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
            },
            ChatStatus::Error => {
                warn!(
                    ticket_id,
                    role, "Forge chat entered error status — preserving session for retry"
                );
                store.set(&action_key, json!("resume_needed")).await;
            }
            ChatStatus::RequiresAction => {
                info!(
                    ticket_id,
                    role, "Forge chat requires_action — setting awaiting_human"
                );
            }
            ChatStatus::Pending => {
                debug!(ticket_id, role, "Forge chat pending");
            }
        }
    }

    /// Read the harness-written status for a ticket.
    /// Returns the phase if set (planning, building, testing, review_ready, blocked).
    ///
    /// NOTE: the harness writes the status as a JSON *object*
    /// (`{ "phase": ..., "role": ..., "ts": ... }`), so we deserialize it
    /// directly into `HarnessStatus` via `store.get_typed`. Deserialising
    /// through `get_typed::<String>` fails — an object cannot be decoded into
    /// a `String` — which used to make this always return `None`, so FORGE
    /// never detected the `planning`/`review_ready` phases and SENTINEL was
    /// never spawned via the planning-gate routing.
    async fn read_harness_status(store: &SharedStore, ticket_id: &str) -> Option<HarnessStatus> {
        let status_key = full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS);
        store.get_typed(&status_key).await
    }

    /// Read the harness-written PR info for a ticket.
    /// This is written when the agent calls `openflows-harness pr opened`.
    ///
    /// Like `read_harness_status`, the PR payload is stored as a JSON object,
    /// so we deserialize directly into `HarnessPrInfo` rather than through
    /// `get_typed::<String>` (which always failed to decode an object).
    async fn read_harness_pr_info(store: &SharedStore, ticket_id: &str) -> Option<HarnessPrInfo> {
        let pr_key = full_ticket_key_flat(ticket_id, "pr");
        store.get_typed(&pr_key).await
    }

    /// Sync harness-written PR info to the global pending_prs list.
    /// This ensures that PRs recorded by the harness are picked up by VESSEL.
    async fn sync_harness_pr_to_pending(
        store: &SharedStore,
        ticket_id: &str,
        worker_id: &str,
        pr_info: &HarnessPrInfo,
    ) {
        let mut pending_prs: Vec<Value> =
            store.get_typed(KEY_PENDING_PRS).await.unwrap_or_default();

        // Check if this PR is already in pending_prs
        let already_tracked = pending_prs
            .iter()
            .any(|p| p["number"].as_u64() == Some(pr_info.pr_number));

        if !already_tracked {
            info!(
                ticket_id,
                pr_number = pr_info.pr_number,
                branch = %pr_info.branch,
                "Syncing harness PR info to pending_prs"
            );

            pending_prs.push(json!({
                "number": pr_info.pr_number,
                "ticket_id": ticket_id,
                "head_branch": pr_info.branch,
                "title": pr_info.title,
                "worker_id": worker_id,
                "source": "harness",
            }));

            store.set(KEY_PENDING_PRS, json!(pending_prs)).await;
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

                let workspace_id = slots.get(&worker_id).and_then(|s| s.workspace_id.clone());

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
        debug!(
            ticket_id,
            worker_id, "ForgePairNode monitoring forge worker"
        );
        Ok(item)
    }

    async fn post_batch(&self, store: &SharedStore, results: Vec<Result<Value>>) -> Result<Action> {
        if results.is_empty() {
            return Ok(Action::new(Action::NO_TICKETS));
        }

        let client = Self::coder_client_from_store(store).await;
        let mut has_pr_opened = false;
        let mut has_review_ready = false;
        let mut has_planning_gate = false;
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
            let worker_id = item["worker_id"].as_str().unwrap_or("");
            let role = "forge";

            // === NEW: Read harness-written status ===
            // NOTE: Only log at info level for significant state transitions.
            // Normal in-progress phases (planning/building/testing) are debug-only
            // to avoid filling logs with per-poll routine output.
            if let Some(harness_status) = Self::read_harness_status(store, ticket_id).await {
                debug!(
                    ticket_id,
                    worker_id,
                    phase = %harness_status.phase,
                    "Read harness status"
                );

                match harness_status.phase.as_str() {
                    "review_ready" => {
                        info!(
                            ticket_id,
                            worker_id, "Harness reports review_ready — checking for PR info"
                        );

                        // Check if harness wrote PR info
                        if let Some(pr_info) = Self::read_harness_pr_info(store, ticket_id).await {
                            info!(
                                ticket_id,
                                pr_number = pr_info.pr_number,
                                "Found harness PR info — syncing to pending_prs"
                            );
                            Self::sync_harness_pr_to_pending(store, ticket_id, worker_id, &pr_info)
                                .await;

                            // Event-driven re-arm signal for VESSEL: if this PR was the
                            // subject of a VESSEL-dispatched `/address_review`, write a
                            // marker so VESSEL re-polls it even if NEXUS would otherwise
                            // skip re-adding it (e.g. ticket in a Failed/InProgress state).
                            if store
                                .get(&address_review_dispatched_key(pr_info.pr_number))
                                .await
                                .is_some()
                            {
                                store
                                    .set(
                                        &address_review_rearmed_key(pr_info.pr_number),
                                        json!(true),
                                    )
                                    .await;
                                info!(
                                    ticket_id,
                                    pr_number = pr_info.pr_number,
                                    "Wrote /address_review re-arm marker for VESSEL"
                                );
                            }

                            has_pr_opened = true;
                        } else {
                            // No PR info but review_ready — signal for Sentinel spawn
                            has_review_ready = true;
                        }
                    }
                    "blocked" => {
                        warn!(ticket_id, worker_id, "Harness reports blocked status");
                        has_failed = true;
                    }
                    "planning" => {
                        // FORGE is in the planning gate — waiting for SENTINEL to
                        // review the plan and approve the gate. Route to NEXUS so
                        // it can spawn a SENTINEL chat for plan review.
                        info!(
                            ticket_id,
                            worker_id,
                            "Harness reports planning phase — SENTINEL review needed for gate approval"
                        );
                        has_planning_gate = true;
                    }
                    "building" | "testing" => {
                        debug!(
                            ticket_id,
                            worker_id,
                            phase = %harness_status.phase,
                            "Harness reports work in progress"
                        );
                        has_in_progress = true;
                    }
                    _ => {}
                }
            }

            // === Also check Coder chat status (existing logic) ===
            let chat_key = full_ticket_key(ticket_id, KEY_TICKET_CHAT, role);
            let chat_id: Option<String> = store.get_typed(&chat_key).await;

            if let (Some(ref client), Some(chat_id)) = (&client, chat_id) {
                match client.get_chat(&chat_id).await {
                    Ok(chat) => {
                        let status = chat.status();

                        // Only emit a full diagnostic warn when this is the first time we
                        // are seeing this chat in error state. Once chat_action is already
                        // set to a resume marker, we have already processed it and every
                        // subsequent poll would re-log the same stale data — degrade to
                        // debug to keep logs actionable.
                        if matches!(status, ChatStatus::Error) {
                            let action_key =
                                full_ticket_key(ticket_id, KEY_TICKET_CHAT_ACTION, role);
                            let last_action: Option<String> = store.get_typed(&action_key).await;
                            let first_sighting = last_action
                                .as_deref()
                                .map(|a| {
                                    !matches!(
                                        a,
                                        "interrupted"
                                            | "resume_needed"
                                            | "resume_failed"
                                            | "first_error_logged"
                                    )
                                })
                                .unwrap_or(true);

                            if first_sighting {
                                // Log enough context to identify and debug the dead chat, but
                                // NOT the message body itself. Coder chat messages can carry
                                // private ticket data, source code, tool output, or credentials;
                                // emitting `content_raw` verbatim would leak all of that into the
                                // controller log surface and any downstream log retention system.
                                // Operators who need the full message can fetch it from the Coder
                                // API via the message id reported here.
                                let last_msg = client
                                    .get_chat_messages(&chat_id, 1)
                                    .await
                                    .ok()
                                    .and_then(|m| m.first().cloned());
                                store.set(&action_key, json!("first_error_logged")).await;
                                let last_message_id =
                                    last_msg.as_ref().map(|m| m.id.as_str()).unwrap_or("");
                                let last_message_role =
                                    last_msg.as_ref().map(|m| m.role.as_str()).unwrap_or("");
                                let last_message_bytes = last_msg
                                    .as_ref()
                                    .map(|m| m.content_raw.to_string().len())
                                    .unwrap_or(0);
                                warn!(
                                    chat_id = %chat_id,
                                    ticket_id,
                                    workspace_id = %chat.workspace_id,
                                    status = ?status,
                                    owner_id = %chat.owner_id,
                                    last_message_id = last_message_id,
                                    last_message_role = last_message_role,
                                    last_message_bytes = last_message_bytes,
                                    "Forge chat entered error state — will be resumed in the same session"
                                );
                            } else {
                                debug!(
                                    chat_id = %chat_id,
                                    ticket_id,
                                    status = ?status,
                                    "Forge chat still in error; already reported"
                                );
                            }
                        }

                        Self::sync_chat_status_to_store(store, ticket_id, role, status).await;
                    }
                    Err(e) => {
                        debug!(
                            chat_id = %chat_id,
                            ticket_id,
                            error = %e,
                            "Failed to get forge chat status (will skip sync for this poll)"
                        );
                    }
                }
            }

            // === Check pending_prs (existing logic) ===
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
                        // Only mark as in_progress if we haven't already found a more specific state
                        if !has_pr_opened && !has_review_ready && !has_planning_gate && !has_failed
                        {
                            has_in_progress = true;
                        }
                    }
                }
            }
        }

        info!(
            monitored = results.len(),
            has_pr_opened,
            has_review_ready,
            has_planning_gate,
            has_failed,
            has_in_progress,
            "ForgePairNode post_batch summary"
        );

        // Priority: PR opened > planning gate > review ready > failed > in progress
        if has_pr_opened {
            info!("Forge: PR(s) opened — routing to sentinel for review");
            Ok(Action::new(ACTION_PR_OPENED))
        } else if has_planning_gate {
            info!("Forge: planning gate — routing to nexus for SENTINEL plan review");
            Ok(Action::new(ACTION_PLANNING_GATE))
        } else if has_review_ready {
            info!("Forge: review_ready without PR — routing to trigger Sentinel spawn");
            Ok(Action::new(ACTION_REVIEW_READY))
        } else if has_failed {
            info!("Forge: failure detected — routing back to nexus");
            Ok(Action::new(ACTION_FAILED))
        } else if has_in_progress {
            info!("Forge: work still in progress — pausing until the next controller poll");
            Ok(Action::new(PAUSE_SIGNAL))
        } else {
            Ok(Action::new(PAUSE_SIGNAL))
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

    async fn post_batch(&self, store: &SharedStore, results: Vec<Result<Value>>) -> Result<Action> {
        self.inner.post_batch(store, results).await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression test: the harness writes the status as a JSON *object*
    /// (e.g. `{"phase":"planning","role":"forge","ts":...}`). Reading it back
    /// through `get_typed::<String>` silently fails (an object cannot decode
    /// into a `String`), which used to make `read_harness_status` always
    /// return `None` — FORGE never noticed the `planning`/`review_ready`
    /// phases, so SENTINEL was never spawned via the planning-gate routing.
    #[tokio::test]
    async fn read_harness_status_decodes_object_stored_status() {
        let store = SharedStore::new_in_memory();
        let ticket_id = "T-048";

        // Simulate what openflows-harness `status set planning` stores.
        store
            .set(
                &full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS),
                json!({
                    "phase": "planning",
                    "role": "forge",
                    "ts": 1717000000u64,
                }),
            )
            .await;

        let status = ForgePairNode::read_harness_status(&store, ticket_id).await;
        let status = status.expect("read_harness_status should decode the object-stored status");
        assert_eq!(status.phase, "planning");
        assert_eq!(status.role, "forge");
        assert_eq!(status.ts, 1717000000);
    }

    #[tokio::test]
    async fn read_harness_status_returns_none_when_phase_missing() {
        let store = SharedStore::new_in_memory();
        let ticket_id = "T-049";

        // An object without a phase should not be returned as a status.
        store
            .set(
                &full_ticket_key_flat(ticket_id, KEY_TICKET_STATUS),
                json!({ "role": "forge", "ts": 1u64 }),
            )
            .await;

        assert!(ForgePairNode::read_harness_status(&store, ticket_id)
            .await
            .is_none());
    }

    #[tokio::test]
    async fn read_harness_pr_info_decodes_object_stored_pr() {
        let store = SharedStore::new_in_memory();
        let ticket_id = "T-050";

        // Simulate what openflows-harness `pr opened` stores.
        store
            .set(
                &full_ticket_key_flat(ticket_id, "pr"),
                json!({
                    "pr_number": 42u64,
                    "branch": "forge-1/T-050",
                    "title": "Implement feature",
                }),
            )
            .await;

        let pr = ForgePairNode::read_harness_pr_info(&store, ticket_id).await;
        let pr = pr.expect("read_harness_pr_info should decode the object-stored PR");
        assert_eq!(pr.pr_number, 42);
        assert_eq!(pr.branch, "forge-1/T-050");
        assert_eq!(pr.title, "Implement feature");
    }
}
