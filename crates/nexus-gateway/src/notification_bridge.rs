// crates/nexus-gateway/src/notification_bridge.rs
//
// NotificationBridge — bridges SharedStore events to gateway notifications.
//
// VesselNotifier and ForgeNode emit events to the SharedStore's ring buffer
// via `store.emit()`. This bridge polls those events and routes them as
// OutboundMessages through the gateway to Discord (and any other channel).
//
// This decouples domain logic (forge, vessel, pair-harness) from
// notification delivery. Nodes just emit store events; the bridge
// handles the gateway routing.

use pocketflow_core::SharedStore;
use tracing::{info, warn};

use crate::gateway::Gateway;
use crate::messages::{OutboundMessage, OutboundMessageType};

/// Maps a `StoreEvent` (agent + event_type + payload) to an `OutboundMessage`.
/// Returns `None` for events that should NOT generate a stakeholder notification
/// (e.g. internal bookkeeping events).
fn map_event_to_message(agent: &str, event_type: &str, payload: &serde_json::Value) -> Option<OutboundMessage> {
    match (agent, event_type) {
        // ── Vessel events ────────────────────────────────────────────
        ("vessel", "ticket_merged") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let pr_number = payload["pr_number"].as_u64().unwrap_or(0);
            let pr_title = payload["pr_title"].as_str().unwrap_or("PR merged");
            Some(OutboundMessage {
                message_type: OutboundMessageType::PrMerged,
                target_channel: None,
                target_conversation: None,
                content: format!("{} merged via PR #{}", ticket_id, pr_number),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: None,
                metadata: serde_json::json!({
                    "pr_number": pr_number,
                    "pr_title": pr_title,
                }),
            })
        }
        ("vessel", "ci_failed") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let pr_number = payload["pr_number"].as_u64().unwrap_or(0);
            let reason = payload["reason"].as_str().unwrap_or("Unknown CI failure");
            Some(OutboundMessage {
                message_type: OutboundMessageType::CiFailed,
                target_channel: None,
                target_conversation: None,
                content: format!("CI failed for PR #{} ({})", pr_number, reason),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: None,
                metadata: serde_json::json!({
                    "pr_number": pr_number,
                    "reason": reason,
                }),
            })
        }
        ("vessel", "merge_blocked") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let pr_number = payload["pr_number"].as_u64().unwrap_or(0);
            let reason = payload["reason"].as_str().unwrap_or("Unknown reason");
            Some(OutboundMessage {
                message_type: OutboundMessageType::MergeBlocked,
                target_channel: None,
                target_conversation: None,
                content: format!("Merge blocked for PR #{}: {}", pr_number, reason),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: None,
                metadata: serde_json::json!({
                    "pr_number": pr_number,
                    "reason": reason,
                }),
            })
        }
        ("vessel", "ci_timeout") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let pr_number = payload["pr_number"].as_u64().unwrap_or(0);
            Some(OutboundMessage {
                message_type: OutboundMessageType::CiTimeout,
                target_channel: None,
                target_conversation: None,
                content: format!("CI timed out for PR #{}", pr_number),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: None,
                metadata: serde_json::json!({
                    "pr_number": pr_number,
                }),
            })
        }
        ("vessel", "ci_missing") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let pr_number = payload["pr_number"].as_u64().unwrap_or(0);
            Some(OutboundMessage {
                message_type: OutboundMessageType::CiMissing,
                target_channel: None,
                target_conversation: None,
                content: format!("No CI workflows for PR #{} — merged without validation", pr_number),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: None,
                metadata: serde_json::json!({
                    "pr_number": pr_number,
                }),
            })
        }
        ("vessel", "conflicts_detected") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let pr_number = payload["pr_number"].as_u64().unwrap_or(0);
            let conflicted_files = payload["conflicted_files"].as_array();
            Some(OutboundMessage {
                message_type: OutboundMessageType::ConflictsDetected,
                target_channel: None,
                target_conversation: None,
                content: format!(
                    "Merge conflicts on PR #{} ({} files)",
                    pr_number,
                    conflicted_files.map(|f| f.len()).unwrap_or(0)
                ),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: None,
                metadata: serde_json::json!({
                    "pr_number": pr_number,
                    "conflicted_files": conflicted_files.cloned().unwrap_or_default(),
                }),
            })
        }

        // ── Forge events ────────────────────────────────────────────
        ("forge", "work_completed") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let worker_id = payload["worker_id"].as_str().unwrap_or("?");
            let outcome = payload["outcome"].as_str().unwrap_or("unknown");
            let pr_number = payload["pr_number"].as_u64().unwrap_or(0);
            let branch = payload["branch"].as_str().unwrap_or("");

            if outcome == "pr_opened" || pr_number > 0 {
                Some(OutboundMessage {
                    message_type: OutboundMessageType::PrOpened,
                    target_channel: None,
                    target_conversation: None,
                    content: format!("{} opened PR #{} for {}", worker_id, pr_number, ticket_id),
                    ticket_id: Some(ticket_id.to_string()),
                    worker_id: Some(worker_id.to_string()),
                    metadata: serde_json::json!({
                        "pr_number": pr_number,
                        "branch": branch,
                    }),
                })
            } else {
                Some(OutboundMessage {
                    message_type: OutboundMessageType::AgentCompleted,
                    target_channel: None,
                    target_conversation: None,
                    content: format!("{} completed {} ({})", worker_id, ticket_id, outcome),
                    ticket_id: Some(ticket_id.to_string()),
                    worker_id: Some(worker_id.to_string()),
                    metadata: serde_json::json!({}),
                })
            }
        }
        ("forge", "work_suspended") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let worker_id = payload["worker_id"].as_str().unwrap_or("?");
            let reason = payload["reason"].as_str().unwrap_or("needs approval");
            Some(OutboundMessage {
                message_type: OutboundMessageType::WorkerSuspended,
                target_channel: None,
                target_conversation: None,
                content: format!("{} suspended on {}: {}", worker_id, ticket_id, reason),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: Some(worker_id.to_string()),
                metadata: serde_json::json!({}),
            })
        }
        ("forge", "work_failed") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let worker_id = payload["worker_id"].as_str().unwrap_or("?");
            let reason = payload["reason"].as_str().unwrap_or("unknown");
            Some(OutboundMessage {
                message_type: OutboundMessageType::TicketFailed,
                target_channel: None,
                target_conversation: None,
                content: format!("{} failed on {}: {}", worker_id, ticket_id, reason),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: Some(worker_id.to_string()),
                metadata: serde_json::json!({}),
            })
        }
        ("forge", "ticket_exhausted") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let worker_id = payload["worker_id"].as_str().unwrap_or("?");
            let attempts = payload["attempts"].as_u64().unwrap_or(0);
            Some(OutboundMessage {
                message_type: OutboundMessageType::TicketExhausted,
                target_channel: None,
                target_conversation: None,
                content: format!("{} exceeded max attempts ({}) on {}", worker_id, attempts, ticket_id),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: Some(worker_id.to_string()),
                metadata: serde_json::json!({}),
            })
        }
        ("forge", "fuel_exhausted") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let worker_id = payload["worker_id"].as_str().unwrap_or("?");
            let reason = payload["reason"].as_str().unwrap_or("timeout");
            Some(OutboundMessage {
                message_type: OutboundMessageType::FuelExhausted,
                target_channel: None,
                target_conversation: None,
                content: format!("{} ran out of fuel on {}: {}", worker_id, ticket_id, reason),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: Some(worker_id.to_string()),
                metadata: serde_json::json!({}),
            })
        }

        // ── Nexus events ─────────────────────────────────────────────
        // Nexus emits structured events via store.emit() instead of
        // calling gateway.broadcast() directly. This keeps all notification
        // routing in one place (the bridge).
        ("nexus", "work_assigned") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let worker_id = payload["worker_id"].as_str().unwrap_or("?");
            let notes = payload["notes"].as_str().unwrap_or("");
            let issue_url = payload["issue_url"].as_str();
            Some(OutboundMessage {
                message_type: OutboundMessageType::WorkflowStarted,
                target_channel: None,
                target_conversation: None,
                content: notes.to_string(),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: Some(worker_id.to_string()),
                metadata: serde_json::json!({
                    "issue_url": issue_url,
                }),
            })
        }
        ("nexus", "worker_assigned") => {
            let ticket_id = payload["ticket_id"].as_str().unwrap_or("?");
            let worker_id = payload["worker_id"].as_str().unwrap_or("?");
            Some(OutboundMessage {
                message_type: OutboundMessageType::AgentAssigned,
                target_channel: None,
                target_conversation: None,
                content: format!("{} has been assigned to work on {}", worker_id, ticket_id),
                ticket_id: Some(ticket_id.to_string()),
                worker_id: Some(worker_id.to_string()),
                metadata: serde_json::json!({}),
            })
        }
        ("nexus", "command_decision") => {
            let worker_id = payload["worker_id"].as_str().unwrap_or("?");
            let decision = payload["decision"].as_str().unwrap_or("unknown");
            Some(OutboundMessage {
                message_type: OutboundMessageType::StatusUpdate,
                target_channel: None,
                target_conversation: None,
                content: format!("Command {} for {}", decision, worker_id),
                ticket_id: None,
                worker_id: Some(worker_id.to_string()),
                metadata: serde_json::json!({}),
            })
        }
        ("nexus", "merge_routing") => {
            let pr_count = payload["pr_count"].as_u64().unwrap_or(0);
            Some(OutboundMessage {
                message_type: OutboundMessageType::StatusUpdate,
                target_channel: None,
                target_conversation: None,
                content: format!("Routing {} pending PR(s) to VESSEL for merge", pr_count),
                ticket_id: None,
                worker_id: None,
                metadata: serde_json::json!({}),
            })
        }
        ("nexus", _) => None,

        // ── Gateway echo events ───────────────────────────────────────
        ("nexus_gateway", "message_sent") => None,

        // ── Unknown events ───────────────────────────────────────────
        _ => None,
    }
}

/// Run the notification bridge as a background task.
///
/// Polls the SharedStore event ring buffer for new events, maps them to
/// OutboundMessages, and routes them through the gateway to all registered
/// channel plugins (Discord, Slack, etc.).
///
/// Call this from a `tokio::spawn` in the main binary.
pub async fn run_bridge(store: SharedStore, gateway: std::sync::Arc<Gateway>) {
    let mut cursor = store.event_count().await;
    info!(
        cursor,
        "NotificationBridge starting — forwarding store events to gateway"
    );

    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        interval.tick().await;

        let events = store.get_events_since(cursor).await;
        if events.is_empty() {
            continue;
        }

        let new_cursor = cursor + events.len();

        for event in &events {
            if let Some(msg) = map_event_to_message(&event.agent, &event.event_type, &event.payload) {
                info!(
                    agent = %event.agent,
                    event_type = %event.event_type,
                    message_type = ?msg.message_type,
                    "NotificationBridge routing store event to gateway"
                );
                if let Err(e) = gateway.broadcast(&msg).await {
                    warn!(
                        agent = %event.agent,
                        event_type = %event.event_type,
                        error = %e,
                        "NotificationBridge failed to broadcast notification"
                    );
                }
            }
        }

        cursor = new_cursor;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pocketflow_core::SharedStore;

    #[test]
    fn test_map_vessel_ticket_merged() {
        let payload = serde_json::json!({
            "ticket_id": "T-001",
            "pr_number": 42,
            "sha": "abc123",
            "pr_title": "Fix login",
            "pr_body": null,
        });
        let msg = map_event_to_message("vessel", "ticket_merged", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::PrMerged);
        assert_eq!(msg.ticket_id.as_deref(), Some("T-001"));
        assert_eq!(msg.metadata["pr_number"], 42);
    }

    #[test]
    fn test_map_vessel_ci_failed() {
        let payload = serde_json::json!({
            "ticket_id": "T-002",
            "pr_number": 10,
            "reason": "Tests failed",
        });
        let msg = map_event_to_message("vessel", "ci_failed", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::CiFailed);
        assert_eq!(msg.metadata["reason"], "Tests failed");
    }

    #[test]
    fn test_map_vessel_conflicts_detected() {
        let payload = serde_json::json!({
            "ticket_id": "T-003",
            "pr_number": 15,
            "conflicted_files": ["src/main.rs", "lib.rs"],
        });
        let msg = map_event_to_message("vessel", "conflicts_detected", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::ConflictsDetected);
    }

    #[test]
    fn test_map_forge_pr_opened() {
        let payload = serde_json::json!({
            "ticket_id": "T-004",
            "worker_id": "forge-1",
            "outcome": "pr_opened",
            "pr_number": 99,
            "branch": "forge-1/T-004",
        });
        let msg = map_event_to_message("forge", "work_completed", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::PrOpened);
        assert_eq!(msg.metadata["pr_number"], 99);
    }

    #[test]
    fn test_map_forge_suspended() {
        let payload = serde_json::json!({
            "ticket_id": "T-005",
            "worker_id": "forge-2",
            "reason": "needs approval",
        });
        let msg = map_event_to_message("forge", "work_suspended", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::WorkerSuspended);
    }

    #[test]
    fn test_map_forge_failed() {
        let payload = serde_json::json!({
            "ticket_id": "T-006",
            "worker_id": "forge-3",
            "reason": "STATUS.json not written",
        });
        let msg = map_event_to_message("forge", "work_failed", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::TicketFailed);
    }

    #[test]
    fn test_map_forge_exhausted() {
        let payload = serde_json::json!({
            "ticket_id": "T-007",
            "worker_id": "forge-1",
            "attempts": 3,
        });
        let msg = map_event_to_message("forge", "ticket_exhausted", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::TicketExhausted);
    }

    #[test]
    fn test_map_forge_fuel_exhausted() {
        let payload = serde_json::json!({
            "ticket_id": "T-008",
            "worker_id": "forge-2",
            "reason": "timeout",
        });
        let msg = map_event_to_message("forge", "fuel_exhausted", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::FuelExhausted);
    }

    #[test]
    fn test_map_nexus_work_assigned() {
        let payload = serde_json::json!({
            "ticket_id": "T-001",
            "worker_id": "forge-1",
            "notes": "Implement login",
            "issue_url": "https://github.com/org/repo/issues/1",
        });
        let msg = map_event_to_message("nexus", "work_assigned", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::WorkflowStarted);
        assert_eq!(msg.ticket_id.as_deref(), Some("T-001"));
    }

    #[test]
    fn test_map_nexus_worker_assigned() {
        let payload = serde_json::json!({
            "ticket_id": "T-001",
            "worker_id": "forge-1",
        });
        let msg = map_event_to_message("nexus", "worker_assigned", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::AgentAssigned);
    }

    #[test]
    fn test_map_nexus_command_decision() {
        let payload = serde_json::json!({
            "worker_id": "forge-2",
            "decision": "approved",
        });
        let msg = map_event_to_message("nexus", "command_decision", &payload).unwrap();
        assert_eq!(msg.message_type, OutboundMessageType::StatusUpdate);
    }

    #[test]
    fn test_map_nexus_unknown_event_skipped() {
        let payload = serde_json::json!({});
        assert!(map_event_to_message("nexus", "internal_sync", &payload).is_none());
    }

    #[test]
    fn test_map_gateway_echo_skipped() {
        let payload = serde_json::json!({});
        assert!(map_event_to_message("nexus_gateway", "message_sent", &payload).is_none());
    }

    #[tokio::test]
    async fn test_bridge_forwards_events() {
        use crate::plugin::ChannelPlugin;
        use async_trait::async_trait;
        use tokio::sync::{mpsc, watch};
        use std::sync::{Arc, Mutex};

        // Create a capturing plugin to verify messages received
        struct CapturePlugin {
            messages: Arc<Mutex<Vec<String>>>,
        }

        #[async_trait]
        impl ChannelPlugin for CapturePlugin {
            fn channel_id(&self) -> &str { "capture" }
            async fn start_listener(
                &self,
                _tx: mpsc::Sender<crate::messages::InboundMessage>,
                _shutdown: watch::Receiver<bool>,
            ) -> anyhow::Result<()> { Ok(()) }
            async fn send(&self, msg: &OutboundMessage) -> anyhow::Result<()> {
                self.messages.lock().unwrap().push(msg.content.clone());
                Ok(())
            }
            async fn ask_human(
                &self, _q: &str, _opts: &[&str], _ticket: &str, _timeout: u64,
            ) -> Option<String> { None }
        }

        let store = SharedStore::new_in_memory();
        let captured = Arc::new(Mutex::new(Vec::new()));
        let mut gateway = Gateway::new(store.clone());
        gateway.register_plugin(Arc::new(CapturePlugin { messages: captured.clone() }));
        let gateway = Arc::new(gateway);

        // Emit a vessel ticket_merged event
        store.emit(
            "vessel",
            "ticket_merged",
            serde_json::json!({
                "ticket_id": "T-100",
                "pr_number": 50,
                "sha": "deadbeef",
                "pr_title": "Test PR",
                "pr_body": null,
            }),
        ).await;

        // Run bridge for one tick
        let store_clone = store.clone();
        let gateway_clone = gateway.clone();
        tokio::spawn(async move {
            let mut interval = tokio::time::interval(std::time::Duration::from_millis(50));
            interval.tick().await;
            let events = store_clone.get_events_since(0).await;
            for event in &events {
                if let Some(msg) = map_event_to_message(&event.agent, &event.event_type, &event.payload) {
                    let _ = gateway_clone.broadcast(&msg).await;
                }
            }
        });

        tokio::time::sleep(std::time::Duration::from_millis(200)).await;

        let msgs = captured.lock().unwrap();
        assert!(!msgs.is_empty(), "Bridge should have forwarded the event");
        assert!(msgs[0].contains("T-100"), "Message should reference ticket T-100");
    }
}
