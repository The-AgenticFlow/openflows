// crates/pocketflow-core/src/flow.rs
//
// Flow — Sprint state machine. Connects Nodes by Action strings.
// All possible state transitions are visible in main.rs wiring.
// No hidden routing logic anywhere else.
//
// Error Recovery: When a node fails (exec phase or other), the flow
// catches the error and routes to a "node_error" handler if one is
// configured, or falls back to a safe cycle (re-enters the current
// node for retry) when no error route exists. This prevents the entire
// orchestration from crashing due to a single transient failure.

use anyhow::{bail, Result};
use std::{collections::HashMap, sync::Arc};
use tracing::{info, warn};

use crate::{Action, Node, SharedStore};

// ── Route map: Action string → next node name ─────────────────────────────

type Routes = HashMap<String, String>; // action → node_name

struct FlowNode {
    node: Arc<dyn Node>,
    routes: Routes,
}

// ── Flow ─────────────────────────────────────────────────────────────────

pub struct Flow {
    start: String,
    nodes: HashMap<String, FlowNode>,
    max_steps: usize, // safety cap to avoid infinite loops in production
    max_retries: usize, // max retries on node error before giving up (default: 3)
}

/// Tracks consecutive failures on a single node for retry-backoff.
struct RetryTracker {
    /// Node name that's been retrying.
    node_name: String,
    /// Number of consecutive failures.
    count: usize,
}

impl Flow {
    pub fn new(start: impl Into<String>) -> Self {
        Self {
            start: start.into(),
            nodes: HashMap::new(),
            max_steps: 10_000,
            max_retries: 3,
        }
    }

    /// Override safety cap (default 10 000 steps).
    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }

    /// Override max retries on node error (default 3).
    pub fn max_retries(mut self, n: usize) -> Self {
        self.max_retries = n;
        self
    }

    /// Register a node with its outgoing route table.
    ///
    /// ```text
    /// // Example (agent crates wired in binary/main.rs):
    /// // flow.add_node("nexus", Arc::new(NexusNode),
    /// //     vec![
    /// //         (Action::TICKETS_READY,  "forge_pool"),
    /// //         (Action::AWAITING_HUMAN, "nexus"),
    /// //         (Action::NO_TICKETS,     "nexus"),
    /// //     ],
    /// // );
    /// ```
    pub fn add_node(
        mut self,
        name: impl Into<String>,
        node: Arc<dyn Node>,
        routes: Vec<(&'static str, &'static str)>,
    ) -> Self {
        let route_map: Routes = routes
            .into_iter()
            .map(|(action, target)| (action.to_string(), target.to_string()))
            .collect();

        self.nodes.insert(
            name.into(),
            FlowNode {
                node,
                routes: route_map,
            },
        );
        self
    }

    /// Run the flow until a node returns the STOP_SIGNAL action or
    /// the step limit is reached. Returns the final Action.
    ///
    /// On node error, the flow attempts recovery:
    /// 1. If a "node_error" route exists for the failed node, route there.
    /// 2. Otherwise, re-enter the same node (self-loop retry).
    /// 3. After max_retries consecutive failures, stop with an error.
    pub async fn run(&self, store: &SharedStore) -> Result<Action> {
        let mut current = self.start.clone();
        let mut steps = 0usize;
        let mut retry_tracker: Option<RetryTracker> = None;

        loop {
            if steps >= self.max_steps {
                bail!(
                    "Flow exceeded max_steps ({}) — possible infinite loop at node '{}'",
                    self.max_steps,
                    current
                );
            }

            let flow_node = self
                .nodes
                .get(&current)
                .ok_or_else(|| anyhow::anyhow!("Flow: unknown node '{}'", current))?;

            info!(step = steps, node = %current, "flow step");

            match flow_node.node.run(store).await {
                Ok(action) => {
                    // Successful execution — clear retry tracker
                    if let Some(ref tracker) = &retry_tracker {
                        if tracker.node_name == current {
                            info!(
                                node = %current,
                                retries = tracker.count,
                                "Node recovered after retries — clearing retry tracker"
                            );
                        }
                        retry_tracker = None;
                    }

                    // Check for stop
                    if action.as_str() == crate::node::STOP_SIGNAL {
                        info!(node = %current, "flow received stop signal");
                        return Ok(action);
                    }

                    // Check for node_error action (explicit error signal from node)
                    if action.as_str() == crate::node::NODE_ERROR {
                        warn!(
                            node = %current,
                            "Node returned node_error action — attempting recovery routing"
                        );
                        if let Some(next) = flow_node.routes.get(crate::node::NODE_ERROR) {
                            info!(from = %current, to = %next, "Routing node_error to recovery node");
                            current = next.clone();
                        } else if let Some(next) = flow_node.routes.get(Action::AWAITING_HUMAN) {
                            info!(
                                from = %current,
                                to = %next,
                                "No node_error route — escalating to awaiting_human"
                            );
                            current = next.clone();
                        } else {
                            // Self-loop retry: re-enter the same node
                            info!(
                                node = %current,
                                "No node_error or awaiting_human route — will retry same node"
                            );
                            // Don't change `current`, just let the loop continue
                        }
                        steps += 1;
                        continue;
                    }

                    // Route to next node
                    match flow_node.routes.get(action.as_str()) {
                        Some(next) => {
                            info!(from = %current, action = action.as_str(), to = %next, "routing");
                            current = next.clone();
                        }
                        None => {
                            warn!(
                                node = %current,
                                action = action.as_str(),
                                "no route defined for action — stopping"
                            );
                            return Ok(action);
                        }
                    }
                }
                Err(e) => {
                    // Node execution failed — attempt recovery
                    warn!(
                        node = %current,
                        error = %e,
                        "Node execution failed — attempting recovery"
                    );

                    // Track consecutive failures for this node
                    let retry_count = match &retry_tracker {
                        Some(tracker) if tracker.node_name == current => tracker.count + 1,
                        _ => 1,
                    };

                    // Emit recovery event to the store for observability
                    store
                        .emit(
                            &current,
                            "node_error_recovery",
                            serde_json::json!({
                                "error": e.to_string(),
                                "retry_count": retry_count,
                                "max_retries": self.max_retries,
                            }),
                        )
                        .await;

                    if retry_count > self.max_retries {
                        bail!(
                            "Node '{}' failed {} consecutive times (max_retries={}) — last error: {}. \
                             This may indicate a persistent issue that needs human intervention.",
                            current,
                            retry_count,
                            self.max_retries,
                            e
                        );
                    }

                    info!(
                        node = %current,
                        retry_count,
                        max_retries = self.max_retries,
                        "Retrying node after failure ({}/{})",
                        retry_count,
                        self.max_retries
                    );

                    retry_tracker = Some(RetryTracker {
                        node_name: current.clone(),
                        count: retry_count,
                    });

                    // Try routing to node_error handler if defined, otherwise self-loop
                    if let Some(next) = flow_node.routes.get(crate::node::NODE_ERROR) {
                        info!(
                            from = %current,
                            to = %next,
                            "Routing failed node to error recovery node"
                        );
                        current = next.clone();
                    } else if let Some(next) = flow_node.routes.get(Action::AWAITING_HUMAN) {
                        info!(
                            from = %current,
                            to = %next,
                            "No node_error route — escalating node failure to awaiting_human"
                        );
                        current = next.clone();
                    }
                    // If no error route, we self-loop: current stays the same,
                    // the node will be retried on the next iteration.
                }
            }

            steps += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Action, SharedStore};
    use async_trait::async_trait;
    use serde_json::Value;
    use std::sync::atomic::{AtomicUsize, Ordering};

    // ── Counter node: increments a store counter, stops at 3 ────────────

    struct CounterNode {
        target: u64,
    }

    #[async_trait]
    impl Node for CounterNode {
        fn name(&self) -> &str {
            "counter"
        }

        async fn prep(&self, store: &SharedStore) -> Result<Value> {
            let n: u64 = store.get_typed("count").await.unwrap_or(0);
            Ok(serde_json::json!(n))
        }

        async fn exec(&self, prep: Value) -> Result<Value> {
            let n = prep.as_u64().unwrap_or(0) + 1;
            Ok(serde_json::json!(n))
        }

        async fn post(&self, store: &SharedStore, result: Value) -> Result<Action> {
            let n = result.as_u64().unwrap_or(0);
            store.set("count", serde_json::json!(n)).await;
            if n >= self.target {
                Ok(Action::new(crate::node::STOP_SIGNAL))
            } else {
                Ok(Action::new("loop"))
            }
        }
    }

    #[tokio::test]
    async fn test_flow_loops_then_stops() {
        let store = SharedStore::new_in_memory();
        let node = Arc::new(CounterNode { target: 3 });

        let flow = Flow::new("counter").add_node("counter", node, vec![("loop", "counter")]);

        let action = flow.run(&store).await.unwrap();
        assert_eq!(action.as_str(), crate::node::STOP_SIGNAL);

        let count: u64 = store.get_typed("count").await.unwrap();
        assert_eq!(count, 3);
    }

    #[tokio::test]
    async fn test_flow_routes_between_nodes() {
        use std::sync::atomic::{AtomicBool, Ordering};

        static B_VISITED: AtomicBool = AtomicBool::new(false);

        struct NodeA;
        #[async_trait]
        impl Node for NodeA {
            fn name(&self) -> &str {
                "a"
            }
            async fn prep(&self, _: &SharedStore) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn exec(&self, _: Value) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn post(&self, _: &SharedStore, _: Value) -> Result<Action> {
                Ok(Action::new("go_b"))
            }
        }

        struct NodeB;
        #[async_trait]
        impl Node for NodeB {
            fn name(&self) -> &str {
                "b"
            }
            async fn prep(&self, _: &SharedStore) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn exec(&self, _: Value) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn post(&self, _: &SharedStore, _: Value) -> Result<Action> {
                B_VISITED.store(true, Ordering::SeqCst);
                Ok(Action::new(crate::node::STOP_SIGNAL))
            }
        }

        let store = SharedStore::new_in_memory();
        let flow = Flow::new("a")
            .add_node("a", Arc::new(NodeA), vec![("go_b", "b")])
            .add_node("b", Arc::new(NodeB), vec![]);

        flow.run(&store).await.unwrap();
        assert!(B_VISITED.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn test_flow_retry_on_node_error() {
        static FAIL_COUNT: AtomicUsize = AtomicUsize::new(0);

        struct FlakyNode;
        #[async_trait]
        impl Node for FlakyNode {
            fn name(&self) -> &str {
                "flaky"
            }
            async fn prep(&self, _: &SharedStore) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn exec(&self, _: Value) -> Result<Value> {
                let count = FAIL_COUNT.fetch_add(1, Ordering::SeqCst);
                if count < 2 {
                    // Fail first 2 times, succeed on 3rd
                    Err(anyhow::anyhow!("Transient failure attempt {}", count + 1))
                } else {
                    Ok(Value::Null)
                }
            }
            async fn post(&self, _: &SharedStore, _: Value) -> Result<Action> {
                Ok(Action::new(crate::node::STOP_SIGNAL))
            }
        }

        let store = SharedStore::new_in_memory();
        let flow = Flow::new("flaky")
            .max_retries(5)
            .add_node("flaky", Arc::new(FlakyNode), vec![]);

        let result = flow.run(&store).await;
        assert!(result.is_ok(), "Flow should recover from transient failures");
        assert_eq!(result.unwrap().as_str(), crate::node::STOP_SIGNAL);
    }
}
