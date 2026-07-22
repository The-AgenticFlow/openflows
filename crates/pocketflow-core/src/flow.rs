// crates/pocketflow-core/src/flow.rs
//
// Flow — Sprint state machine. Connects Nodes by Action strings.
// All possible state transitions are visible in main.rs wiring.
// No hidden routing logic anywhere else.

use anyhow::Result;
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
    max_steps: usize,          // safety cap to avoid infinite loops in production
    max_visits_per_node: usize, // per-node cycle detection threshold
}

impl Flow {
    pub fn new(start: impl Into<String>) -> Self {
        Self {
            start: start.into(),
            nodes: HashMap::new(),
            max_steps: 10_000,
            max_visits_per_node: 20,
        }
    }

    /// Override safety cap (default 10 000 steps).
    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
        self
    }

    /// Override per-node cycle detection threshold (default 20 visits).
    /// When any single node is visited more than this many times in one
    /// flow pass, the flow pauses instead of continuing to spin.  This
    /// catches tight ping-pong cycles (A→B→A→B→…) in ~2×threshold steps
    /// rather than waiting for max_steps.
    pub fn max_visits_per_node(mut self, n: usize) -> Self {
        self.max_visits_per_node = n;
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

    /// Run the flow until a node returns a stop or pause signal, or a
    /// self-healing guard triggers (step limit or per-node cycle detection).
    ///
    /// On any guard, the flow returns a [`PAUSE_SIGNAL`][crate::node::PAUSE_SIGNAL]
    /// action instead of producing a fatal error.  This lets the caller (the
    /// paced controller loop) survive abnormal states and retry on the next
    /// poll — the system self-heals instead of crashing.
    pub async fn run(&self, store: &SharedStore) -> Result<Action> {
        let mut current = self.start.clone();
        let mut steps = 0usize;
        // Per-node visit counts for cycle detection.
        let mut node_visits: HashMap<String, usize> = HashMap::new();

        loop {
            // ── Guard 1: step cap ──────────────────────────────────────────
            if steps >= self.max_steps {
                warn!(
                    max_steps = self.max_steps,
                    node = %current,
                    "Flow reached step cap — pausing for self-healing (caller will retry next poll)"
                );
                return Ok(Action::new(crate::node::PAUSE_SIGNAL));
            }

            // ── Guard 2: per-node cycle detection ─────────────────────────
            let visits = node_visits.entry(current.clone()).or_insert(0);
            *visits += 1;
            if *visits > self.max_visits_per_node {
                warn!(
                    node = %current,
                    visits = *visits,
                    threshold = self.max_visits_per_node,
                    steps,
                    "Node visited {} times in one pass — tight cycle detected, pausing for self-healing",
                    visits
                );
                return Ok(Action::new(crate::node::PAUSE_SIGNAL));
            }

            let flow_node = self
                .nodes
                .get(&current)
                .ok_or_else(|| anyhow::anyhow!("Flow: unknown node '{}'", current))?;

            info!(step = steps, node = %current, "flow step");

            let action = flow_node.node.run(store).await?;

            // Check for a terminal signal before attempting to route it.
            if action.as_str() == crate::node::STOP_SIGNAL {
                info!(node = %current, "flow received stop signal");
                return Ok(action);
            }
            if action.as_str() == crate::node::PAUSE_SIGNAL {
                info!(node = %current, "flow pass paused");
                return Ok(action);
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
    async fn test_flow_pauses_without_routing() {
        struct PauseNode;

        #[async_trait]
        impl Node for PauseNode {
            fn name(&self) -> &str {
                "pause"
            }

            async fn prep(&self, _: &SharedStore) -> Result<Value> {
                Ok(Value::Null)
            }

            async fn exec(&self, _: Value) -> Result<Value> {
                Ok(Value::Null)
            }

            async fn post(&self, _: &SharedStore, _: Value) -> Result<Action> {
                Ok(Action::new(crate::node::PAUSE_SIGNAL))
            }
        }

        let store = SharedStore::new_in_memory();
        let flow = Flow::new("pause").add_node("pause", Arc::new(PauseNode), vec![]);

        let action = flow.run(&store).await.unwrap();
        assert_eq!(action.as_str(), crate::node::PAUSE_SIGNAL);
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

    // ── PingPongNode: always returns the same action, simulating the
    //    nexus<->forge_pair ping-pong that caused the original infinite loop.

    #[tokio::test]
    async fn test_cycle_detection_pauses_ping_pong() {
        struct PingNode {
            label: &'static str,
            action: &'static str,
        }

        #[async_trait]
        impl Node for PingNode {
            fn name(&self) -> &str {
                self.label
            }
            async fn prep(&self, _: &SharedStore) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn exec(&self, _: Value) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn post(&self, _: &SharedStore, _: Value) -> Result<Action> {
                Ok(Action::new(self.action))
            }
        }

        let store = SharedStore::new_in_memory();
        let a = Arc::new(PingNode {
            label: "a",
            action: "bounce_b",
        });
        let b = Arc::new(PingNode {
            label: "b",
            action: "bounce_a",
        });

        let flow = Flow::new("a")
            .max_visits_per_node(3)
            .add_node("a", a, vec![("bounce_b", "b")])
            .add_node("b", b, vec![("bounce_a", "a")]);

        let action = flow.run(&store).await.unwrap();
        // Should pause (not error) after detecting the cycle
        assert_eq!(action.as_str(), crate::node::PAUSE_SIGNAL);
    }

    #[tokio::test]
    async fn test_max_steps_pauses_instead_of_error() {
        struct AlwaysLoop;

        #[async_trait]
        impl Node for AlwaysLoop {
            fn name(&self) -> &str {
                "loop"
            }
            async fn prep(&self, _: &SharedStore) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn exec(&self, _: Value) -> Result<Value> {
                Ok(Value::Null)
            }
            async fn post(&self, _: &SharedStore, _: Value) -> Result<Action> {
                // Always return "continue" → routes back to itself forever
                Ok(Action::new("continue"))
            }
        }

        let store = SharedStore::new_in_memory();
        let node = Arc::new(AlwaysLoop);
        let flow = Flow::new("loop")
            .max_steps(5)
            .max_visits_per_node(100) // set high so max_steps fires first
            .add_node("loop", node, vec![("continue", "loop")]);

        let action = flow.run(&store).await.unwrap();
        // Should pause (not error) when max_steps is hit
        assert_eq!(action.as_str(), crate::node::PAUSE_SIGNAL);
    }

    #[tokio::test]
    async fn test_legitimate_multi_visit_under_threshold() {
        // A node that counts down and loops to itself — should be allowed
        // to visit up to max_visits_per_node times without pause.
        struct CountdownNode;

        #[async_trait]
        impl Node for CountdownNode {
            fn name(&self) -> &str {
                "countdown"
            }
            async fn prep(&self, store: &SharedStore) -> Result<Value> {
                let n: u64 = store.get_typed("n").await.unwrap_or(10);
                Ok(serde_json::json!(n))
            }
            async fn exec(&self, prep: Value) -> Result<Value> {
                let n = prep.as_u64().unwrap_or(0).saturating_sub(1);
                Ok(serde_json::json!(n))
            }
            async fn post(&self, store: &SharedStore, result: Value) -> Result<Action> {
                let n = result.as_u64().unwrap_or(0);
                store.set("n", serde_json::json!(n)).await;
                if n == 0 {
                    Ok(Action::new(crate::node::STOP_SIGNAL))
                } else {
                    Ok(Action::new("loop"))
                }
            }
        }

        let store = SharedStore::new_in_memory();
        store.set("n", serde_json::json!(10)).await;

        let node = Arc::new(CountdownNode);
        // Each pass visits the node once. 10 passes → 10 visits.
        // max_visits_per_node(20) should allow this.
        let flow = Flow::new("countdown")
            .max_visits_per_node(20)
            .add_node("countdown", node, vec![("loop", "countdown")]);

        let action = flow.run(&store).await.unwrap();
        assert_eq!(action.as_str(), crate::node::STOP_SIGNAL);
        let n: u64 = store.get_typed("n").await.unwrap();
        assert_eq!(n, 0);
    }
}
