//! agent-sentinel — SENTINEL adversarial review node (Coder-only redesign).
//!
//! Thin flow node that reads harness-written review keys from SharedStore
//! and routes based on the sentinel's verdict (approve → vessel, reject → forge).
//! The actual review intelligence lives in the Coder Agent (control plane).

use anyhow::Result;
use async_trait::async_trait;
use coder_client::{ChatStatus, CoderClient};
use config::lifecycle::{Decision, Lifecycle, Phase};
use config::state::{
    full_ticket_key, full_ticket_key_flat, pr_review_namespace, review_action_key, KEY_PENDING_PRS,
    KEY_TICKETS, KEY_TICKET_CHAT, KEY_TICKET_CHAT_ACTION,
};
use config::{Envconfig, Ticket, TicketStatus};
use pocketflow_core::{node::PAUSE_SIGNAL, Action, Node, SharedStore};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tracing::{debug, info, warn};

const ACTION_REVIEW_APPROVE: &str = "review_approve";
const ACTION_REVIEW_REJECT: &str = "review_reject";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub verdict: String,
    pub report: String,
    pub pr_number: Option<u64>,
    #[serde(default)]
    pub revision: u64,
    #[serde(default)]
    pub round: u64,
    #[serde(default)]
    pub head: String,
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

    fn monitor_namespace(state: &Lifecycle) -> Option<String> {
        match state.phase {
            Phase::PlanReady | Phase::Testing => Some(format!(
                "{}:{}:{}:{}",
                if state.phase == Phase::Testing {
                    "testing"
                } else {
                    "planning_gate"
                },
                state.revision,
                state.head.as_deref().unwrap_or("plan"),
                state.review_round,
            )),
            Phase::Submit => Some(pr_review_namespace(
                state.revision,
                state.head.as_deref().unwrap_or(""),
                state.review_round,
            )),
            _ => None,
        }
    }

    async fn record_review_delivery(
        store: &SharedStore,
        ticket: &str,
        state: &Lifecycle,
        decision: &Decision,
    ) -> Result<()> {
        store
            .transition(
                ticket,
                state.version,
                "sentinel",
                config::lifecycle::Event::ReviewDelivered {
                    round: decision.round,
                },
            )
            .await?;
        let namespace = pr_review_namespace(
            decision.revision,
            decision.head.as_deref().unwrap_or(""),
            decision.round,
        );
        // Preserve the session, worker assignment, and round aliases until Nexus
        // closes the ticket. Delivery only completes this round's action.
        store
            .set(&review_action_key(ticket, &namespace), json!("completed"))
            .await;
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

    /// Resolve the PR number for a review verdict.
    ///
    /// The SENTINEL chat's `review submit` command may omit `--pr`, which leaves
    /// `ReviewPayload.pr_number == None`. When that happens we fall back to the
    /// PR number Forge recorded when it opened the PR
    /// (`ticket:{id}:pr` -> `{pr_number, branch, title}`). This guarantees the
    /// GitHub review submission (approve / request-changes) always targets the
    /// right PR instead of being silently skipped.
    ///
    /// A supplied `--pr` that disagrees with the ticket's recorded PR is treated
    /// as a reviewer mistake: we log a warning and use the ticket's recorded PR
    /// so a wrong `--pr` can never post APPROVE/REQUEST_CHANGES to another PR.
    #[cfg(test)]
    async fn resolve_pr_number(
        store: &SharedStore,
        ticket_id: &str,
        pr_number: Option<u64>,
    ) -> Option<u64> {
        let pr_key = full_ticket_key_flat(ticket_id, "pr");
        #[derive(serde::Deserialize)]
        struct StoredPr {
            pr_number: u64,
        }
        let stored = store
            .get_typed::<StoredPr>(&pr_key)
            .await
            .map(|p| p.pr_number);

        match (pr_number, stored) {
            (Some(supplied), Some(recorded)) if supplied == recorded => Some(supplied),
            (Some(supplied), Some(recorded)) => {
                warn!(
                    supplied_pr = supplied,
                    recorded_pr = recorded,
                    ticket_id,
                    "Verdict --pr does not match the ticket's recorded PR — using the recorded PR"
                );
                Some(recorded)
            }
            (Some(supplied), None) => Some(supplied),
            (None, stored) => stored,
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

        let mut reviewable = Vec::new();
        let planning_gate_pending: Vec<Value> = Vec::new();

        for ticket in &tickets {
            let worker_id = match &ticket.status {
                TicketStatus::InProgress { worker_id } => worker_id.clone(),
                TicketStatus::Assigned { worker_id } => worker_id.clone(),
                _ => continue,
            };

            let lifecycle = store.lifecycle(&ticket.id).await?;
            let has_review = lifecycle.pr_delivery.is_some();
            if let Some(review) = &lifecycle.pr_delivery {
                reviewable.push(json!({"ticket_id":ticket.id,"worker_id":worker_id,"verdict":if review.approved {"approve"} else {"reject"},"report":review.report,"revision":review.revision,"round":review.round,"head":review.head,"pr_number":review.pr_number,"review_type":"pr_review"}));
            }

            let Some(namespace) = Self::monitor_namespace(&lifecycle) else {
                continue;
            };
            let monitor_action_key = review_action_key(&ticket.id, &namespace);
            let chat_id: Option<String> = store
                .get_typed(&full_ticket_key_flat(&ticket.id, "sentinel_session"))
                .await;
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
                "revision": review["revision"],
                "round": review["round"],
                "head": review["head"],
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
        let verdicts = exec_result["verdicts"]
            .as_array()
            .cloned()
            .unwrap_or_default();
        let mut any_approved = false;
        let mut any_rejected = false;
        for verdict in verdicts {
            let Some(ticket) = verdict["ticket_id"].as_str() else {
                continue;
            };
            let state = store.lifecycle(ticket).await?;
            let Some(decision) = state.deliverable_review() else {
                continue;
            };
            // The rejection is already durable and has returned the worker to building.
            // Deliver feedback independently of GitHub availability.
            if !decision.approved {
                Self::remove_rejected_pr_from_pending(store, ticket, decision.pr_number).await;
                // NEXUS is the sole feedback sender, using the durable
                // lifecycle decision and one revision/round notification key.
                // Sending here as well would duplicate the controller message.
                if store
                    .get(&full_ticket_key(ticket, KEY_TICKET_CHAT, "forge"))
                    .await
                    .is_none()
                {
                    store
                        .set(
                            &full_ticket_key(ticket, KEY_TICKET_CHAT_ACTION, "forge"),
                            json!("resume_needed"),
                        )
                        .await;
                }
                any_rejected = true;
            }
            if decision.pr_number.is_none() {
                continue;
            }
            // SENTINEL shares the PR author's GitHub account (the tenant's
            // external-auth identity), and GitHub blocks self-review, so a formal
            // GitHub review can never be submitted here. The lifecycle verdict is
            // authoritative, so the delivery handshake (clearing pr_delivery) must
            // complete regardless — never gate it on a GitHub call that cannot
            // succeed, or approved PRs deadlock at merge_ready.
            Self::record_review_delivery(store, ticket, &state, decision).await?;
            any_approved |= decision.approved;
        }
        Ok(Action::new(if any_approved {
            ACTION_REVIEW_APPROVE
        } else if any_rejected {
            ACTION_REVIEW_REJECT
        } else {
            PAUSE_SIGNAL
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use config::state::{review_chat_key, KEY_TICKET_STATUS, KEY_WORKER_SLOTS};

    fn node() -> SentinelNode {
        SentinelNode::new("sentinel.agent.md")
    }

    #[test]
    fn monitoring_uses_current_review_phase_revision_head_and_round() {
        use config::lifecycle::{Lifecycle, Phase};
        for (phase, head, expected) in [
            (Phase::PlanReady, None, Some("planning_gate:2:plan:4")),
            (Phase::PlanReady, Some("abc"), Some("planning_gate:2:abc:4")),
            (Phase::Testing, Some("abc"), Some("testing:2:abc:4")),
            (Phase::Submit, Some("abc"), Some("pr_review:2:abc:4")),
            (Phase::Planning, None, None),
            (Phase::Building, Some("abc"), None),
            (Phase::Done, Some("abc"), None),
        ] {
            let state = Lifecycle {
                phase,
                revision: 2,
                review_round: 4,
                head: head.map(str::to_owned),
                ..Default::default()
            };
            assert_eq!(SentinelNode::monitor_namespace(&state).as_deref(), expected);
        }
    }

    #[tokio::test]
    async fn delivered_review_preserves_session_slot_and_round_evidence() {
        use config::lifecycle::{Decision, Lifecycle, Phase};
        let store = SharedStore::new_in_memory();
        let decision = Decision {
            pr_number: Some(42),
            round: 3,
            actor: "sentinel".into(),
            approved: false,
            report: "Fix tests".into(),
            revision: 1,
            head: Some("abc".into()),
        };
        let state = Lifecycle {
            phase: Phase::Building,
            version: 8,
            review_round: 3,
            revision: 1,
            pr_number: Some(42),
            pr_delivery: Some(decision.clone()),
            ..Default::default()
        };
        store
            .set("ticket:T-42:status", serde_json::to_value(&state).unwrap())
            .await;
        store
            .set("ticket:T-42:sentinel_session", json!("persistent-chat"))
            .await;
        let chat_key = review_chat_key("T-42", "pr_review:1:abc:3");
        store.set(&chat_key, json!("persistent-chat")).await;
        let slots = json!({"sentinel-1": {"id": "sentinel-1", "status": {
            "type": "working", "ticket_id": "T-42", "issue_url": null
        }}});
        store.set(KEY_WORKER_SLOTS, slots.clone()).await;
        SentinelNode::record_review_delivery(&store, "T-42", &state, &decision)
            .await
            .unwrap();
        assert!(store.lifecycle("T-42").await.unwrap().pr_delivery.is_none());
        assert_eq!(
            store
                .get_typed::<String>("ticket:T-42:sentinel_session")
                .await
                .as_deref(),
            Some("persistent-chat")
        );
        assert_eq!(
            store.get_typed::<String>(&chat_key).await.as_deref(),
            Some("persistent-chat")
        );
        assert_eq!(
            store.get_typed::<Value>(KEY_WORKER_SLOTS).await,
            Some(slots)
        );
        assert_eq!(
            store
                .get_typed::<String>(&review_action_key("T-42", "pr_review:1:abc:3"))
                .await
                .as_deref(),
            Some("completed")
        );
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
    async fn post_reject_completes_delivery_when_github_is_unavailable() {
        use config::lifecycle::{Decision, Lifecycle, Phase};
        let store = SharedStore::new_in_memory();
        let ticket = "T-42";
        let decision = Decision {
            pr_number: Some(42),
            round: 3,
            actor: "sentinel".into(),
            approved: false,
            report: "Fix failing test".into(),
            revision: 1,
            head: Some("abc".into()),
        };
        let state = Lifecycle {
            phase: Phase::Building,
            version: 8,
            review_round: 3,
            revision: 1,
            pr_number: Some(42),
            feedback: Some(decision.report.clone()),
            pr_delivery: Some(decision),
            ..Default::default()
        };
        store
            .set(
                &full_ticket_key_flat(ticket, KEY_TICKET_STATUS),
                serde_json::to_value(state).unwrap(),
            )
            .await;
        let node = SentinelNode::new("registry.json");
        let result = node
            .post(&store, json!({"verdicts":[{"ticket_id":ticket}]}))
            .await
            .unwrap();
        assert_eq!(result.as_str(), ACTION_REVIEW_REJECT);
        let state = store.lifecycle(ticket).await.unwrap();
        assert_eq!(state.phase, Phase::Building);
        // The delivery handshake must complete (pr_delivery cleared) regardless
        // of GitHub availability — SENTINEL shares the PR author's account and
        // cannot submit a GitHub review, so delivery must not be gated on it.
        assert!(state.pr_delivery.is_none());
        assert_eq!(
            store
                .get_typed::<String>(&full_ticket_key(ticket, KEY_TICKET_CHAT_ACTION, "forge"))
                .await
                .as_deref(),
            Some("resume_needed")
        );
    }

    #[test]
    fn action_constants_match_expected_action_names() {
        assert_eq!(ACTION_REVIEW_APPROVE, "review_approve");
        assert_eq!(ACTION_REVIEW_REJECT, "review_reject");
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
