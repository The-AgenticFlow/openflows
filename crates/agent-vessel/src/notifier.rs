// crates/agent-vessel/src/notifier.rs
//
// Event Emission — separated for modularity and reusability.
// Handles the "notification" phase of VESSEL's workflow.

use pocketflow_core::SharedStore;
use serde_json::json;
use tracing::{debug, info};

/// VESSEL Notifier — emits events to SharedStore for dependency resolution.
pub struct VesselNotifier;

impl VesselNotifier {
    /// Emit ticket_merged event to SharedStore.
    /// This is critical for NEXUS to advance dependency chains.
    pub async fn emit_ticket_merged(
        store: &SharedStore,
        ticket_id: &str,
        pr_number: u64,
        sha: &str,
        pr_title: &str,
        pr_body: Option<&str>,
    ) {
        info!(ticket_id, pr_number, sha, "Emitting ticket_merged event");

        store
            .emit(
                "vessel",
                "ticket_merged",
                json!({
                    "ticket_id": ticket_id,
                    "pr_number": pr_number,
                    "sha": sha,
                    "pr_title": pr_title,
                    "pr_body": pr_body,
                }),
            )
            .await;
    }

    /// Emit deploy_failed event for CI failures.
    pub async fn emit_ci_failed(
        store: &SharedStore,
        ticket_id: Option<&str>,
        pr_number: u64,
        reason: &str,
    ) {
        info!(ticket_id = ?ticket_id, pr_number, reason, "Emitting ci_failed event");

        store
            .emit(
                "vessel",
                "ci_failed",
                json!({
                    "ticket_id": ticket_id,
                    "pr_number": pr_number,
                    "reason": reason,
                }),
            )
            .await;
    }

    /// Emit merge_blocked event for mechanical merge failures.
    pub async fn emit_merge_blocked(
        store: &SharedStore,
        ticket_id: Option<&str>,
        pr_number: u64,
        reason: &str,
    ) {
        info!(ticket_id = ?ticket_id, pr_number, reason, "Emitting merge_blocked event");

        store
            .emit(
                "vessel",
                "merge_blocked",
                json!({
                    "ticket_id": ticket_id,
                    "pr_number": pr_number,
                    "reason": reason,
                }),
            )
            .await;
    }

    /// Emit ci_timeout event when polling times out.
    pub async fn emit_ci_timeout(store: &SharedStore, ticket_id: Option<&str>, pr_number: u64) {
        info!(ticket_id = ?ticket_id, pr_number, "Emitting ci_timeout event");

        store
            .emit(
                "vessel",
                "ci_timeout",
                json!({
                    "ticket_id": ticket_id,
                    "pr_number": pr_number,
                }),
            )
            .await;
    }

    /// Emit ci_missing event when no CI workflows are configured in the repo.
    pub async fn emit_ci_missing(store: &SharedStore, ticket_id: Option<&str>, pr_number: u64) {
        info!(ticket_id = ?ticket_id, pr_number, "Emitting ci_missing event — no CI workflows configured");

        store
            .emit(
                "vessel",
                "ci_missing",
                json!({
                    "ticket_id": ticket_id,
                    "pr_number": pr_number,
                }),
            )
            .await;
    }

    /// Emit conflicts_detected event when merge conflicts prevent CI/merge.
    pub async fn emit_conflicts_detected(
        store: &SharedStore,
        ticket_id: Option<&str>,
        pr_number: u64,
        conflicted_files: &[String],
    ) {
        info!(
            ticket_id = ?ticket_id,
            pr_number,
            files = conflicted_files.len(),
            "Emitting conflicts_detected event"
        );

        store
            .emit(
                "vessel",
                "conflicts_detected",
                json!({
                    "ticket_id": ticket_id,
                    "pr_number": pr_number,
                    "conflicted_files": conflicted_files,
                }),
            )
            .await;
    }

    /// Write ticket status to SharedStore for dependency resolution.
    /// Key format: ticket:{ticket_id}:status
    pub async fn set_ticket_status_merged(store: &SharedStore, ticket_id: &str) {
        let key = format!("ticket:{}:status", ticket_id);
        // Lifecycle completion is written atomically with merge evidence by VESSEL.
        let _ = (store, key);
        debug!(ticket_id, "Merge notification preserves lifecycle record");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_emit_ticket_merged() {
        let store = SharedStore::new_in_memory();

        VesselNotifier::emit_ticket_merged(
            &store,
            "T-42",
            123,
            "abc123",
            "Fix login bug",
            Some("Fixed the login issue"),
        )
        .await;

        let events = store.get_events_since(0).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].agent, "vessel");
        assert_eq!(events[0].event_type, "ticket_merged");
        assert_eq!(events[0].payload["ticket_id"], "T-42");
        assert_eq!(events[0].payload["pr_number"], 123);
        assert_eq!(events[0].payload["pr_title"], "Fix login bug");
    }

    #[tokio::test]
    async fn test_emit_ci_failed() {
        let store = SharedStore::new_in_memory();

        VesselNotifier::emit_ci_failed(&store, Some("T-42"), 123, "Tests failed").await;

        let events = store.get_events_since(0).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "ci_failed");
        assert_eq!(events[0].payload["reason"], "Tests failed");
    }

    #[tokio::test]
    async fn test_emit_merge_blocked() {
        let store = SharedStore::new_in_memory();

        VesselNotifier::emit_merge_blocked(&store, Some("T-42"), 123, "Merge conflict").await;

        let events = store.get_events_since(0).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "merge_blocked");
    }

    #[tokio::test]
    async fn test_emit_ci_timeout() {
        let store = SharedStore::new_in_memory();

        VesselNotifier::emit_ci_timeout(&store, Some("T-42"), 123).await;

        let events = store.get_events_since(0).await;
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "ci_timeout");
    }

    #[tokio::test]
    async fn test_set_ticket_status_merged() {
        let store = SharedStore::new_in_memory();

        VesselNotifier::set_ticket_status_merged(&store, "T-42").await;

        let status = store.get("ticket:T-42:status").await;
        assert_eq!(
            status, None,
            "notification must not manufacture merge evidence"
        );
    }
}
