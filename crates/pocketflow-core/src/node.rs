// crates/pocketflow-core/src/node.rs
//
// The Node trait is the fundamental building block of the flow.
// Each agent (NEXUS, SENTINEL, VESSEL, LORE) implements this trait.

use anyhow::Result;
use async_trait::async_trait;
use serde_json::Value;
use tracing::{error, info};

use crate::{Action, SharedStore};

/// A Node has three phases run in strict sequence by Flow::run_node.
///
/// - `prep`  : Read from SharedStore. Pure read, no side-effects on store.
/// - `exec`  : Do the work (LLM calls, subprocess spawning, GitHub API).
///             MUST NOT write to the store.
/// - `post`  : Write results to store. Return the next Action.
#[async_trait]
pub trait Node: Send + Sync {
    fn name(&self) -> &str;

    /// Phase 1 — Read only.
    async fn prep(&self, store: &SharedStore) -> Result<Value>;

    /// Phase 2 — Pure computation / external I/O. Store is intentionally
    /// not passed here to enforce the no-write contract.
    async fn exec(&self, prep_result: Value) -> Result<Value>;

    /// Phase 3 — Write results, return routing Action.
    async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action>;

    /// Orchestrated by Flow — calls prep → exec → post in sequence.
    /// Emits lifecycle events to the ring buffer throughout.
    async fn run(&self, store: &SharedStore) -> Result<Action> {
        let name = self.name();

        store
            .emit(name, "prep_started", serde_json::json!({}))
            .await;
        let prep_result = self.prep(store).await.map_err(|e| {
            error!(node = name, error = %e, "prep failed");
            e
        })?;
        store.emit(name, "prep_done", serde_json::json!({})).await;

        store
            .emit(name, "exec_started", serde_json::json!({}))
            .await;
        let exec_result = self.exec(prep_result).await.map_err(|e| {
            error!(node = name, error = %e, "exec failed");
            e
        })?;
        store.emit(name, "exec_done", serde_json::json!({})).await;

        store
            .emit(name, "post_started", serde_json::json!({}))
            .await;
        let action = self.post(store, exec_result).await.map_err(|e| {
            error!(node = name, error = %e, "post failed");
            e
        })?;
        store
            .emit(
                name,
                "post_done",
                serde_json::json!({ "action": action.as_str() }),
            )
            .await;

        info!(node = name, action = action.as_str(), "node completed");
        Ok(action)
    }
}

/// Convenience: a no-op prep that returns an empty JSON object.
/// Nodes that don't need to read from the store can use this.
pub async fn noop_prep(_store: &SharedStore) -> Result<Value> {
    Ok(serde_json::json!({}))
}

/// Marker to signal to the Flow that a node requests termination.
pub const STOP_SIGNAL: &str = "__stop__";

/// Marker to end the current flow pass so its caller can poll again later.
pub const PAUSE_SIGNAL: &str = "__pause__";

/// Helper: wrap a string action for use in post() return sites.
#[inline]
pub fn action(s: &str) -> Result<Action> {
    Ok(Action::new(s))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SharedStore;

    struct EchoNode;

    #[async_trait]
    impl Node for EchoNode {
        fn name(&self) -> &str {
            "echo"
        }

        async fn prep(&self, _store: &SharedStore) -> Result<Value> {
            Ok(serde_json::json!({ "input": "hello" }))
        }

        async fn exec(&self, prep_result: Value) -> Result<Value> {
            Ok(prep_result) // echo back
        }

        async fn post(&self, store: &SharedStore, exec_result: Value) -> Result<Action> {
            store.set("output", exec_result).await;
            Ok(Action::new("done"))
        }
    }

    #[tokio::test]
    async fn test_node_lifecycle() {
        let store = SharedStore::new_in_memory();
        let node = EchoNode;

        let action = node.run(&store).await.unwrap();
        assert_eq!(action.as_str(), "done");

        let output = store.get("output").await.unwrap();
        assert_eq!(output["input"], "hello");
    }

    #[tokio::test]
    async fn test_events_emitted() {
        let store = SharedStore::new_in_memory();
        let node = EchoNode;

        node.run(&store).await.unwrap();

        // prep_started, prep_done, exec_started, exec_done, post_started, post_done = 6
        let events = store.get_events_since(0).await;
        assert_eq!(events.len(), 6);
        assert_eq!(events[0].event_type, "prep_started");
        assert_eq!(events[5].event_type, "post_done");
    }
}
