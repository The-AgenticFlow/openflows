// crates/pocketflow-core/src/flow.rs
//
// Flow — Sprint state machine. Connects Nodes by Action strings.
// All possible state transitions are visible in main.rs wiring.
// No hidden routing logic anywhere else.

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
}

impl Flow {
    pub fn new(start: impl Into<String>) -> Self {
        Self {
            start: start.into(),
            nodes: HashMap::new(),
            max_steps: 10_000,
        }
    }

    /// Override safety cap (default 10 000 steps).
    pub fn max_steps(mut self, n: usize) -> Self {
        self.max_steps = n;
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
    pub async fn run(&self, store: &SharedStore) -> Result<Action> {
        let mut current = self.start.clone();
        let mut steps = 0usize;

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

            let action = flow_node.node.run(store).await?;

            // Check for stop
            if action.as_str() == crate::node::STOP_SIGNAL {
                info!(node = %current, "flow received stop signal");
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
}
