// crates/agent-nexus/src/a2a/mod.rs
//! A2A relay server for Sentinel↔Forge delegated verification.
//!
//! Nexus runs an HTTP server this module owns. Sentinel and Forge workspaces
//! communicate with it over JSON-RPC HTTP (POST /rpc); the relay routes
//! verify tasks from Sentinel to the Forge executor for the same pair_id.
//! v1 uses pull-based task delivery: Sentinel submits via `message/send`,
//! Forge claims via `tasks/claim`, executes, and reports via `tasks/complete`;
//! Sentinel polls `tasks/get` for the terminal state. SSE (`GET /`) is
//! reserved for future streaming/push delivery.
//!
//! All terminal results are mirrored to Redis (pair:{pair_id}:verification,
//! audit:a2a:{task_id}:*), keyed under the tenant namespace.
//!
//! Scope (task 2 of the Sentinel↔Forge delegated verification plan,
//! issue #143).

pub(crate) mod http_server;
mod routing;
mod verify_handler;

#[cfg(test)]
mod tests;

pub use http_server::create_router;
pub use routing::{A2ARelay, A2ASession, BufferedEvent, EventBuffer, TaskEntry, TaskState};
pub use verify_handler::submit_verify_request;

use anyhow::Result;
use pocketflow_core::SharedStore;
use std::sync::Arc;
use tracing::info;

/// Create and start the A2A relay HTTP server as a background task.
/// Returns the relay instance for integration with NexusNode if needed.
///
/// Spawns an Axum HTTP server listening on the address specified by
/// A2A_RELAY_ADDR env var (default: 127.0.0.1:3000).
pub async fn start_a2a_relay(store: Arc<SharedStore>) -> Result<Arc<A2ARelay>> {
    let relay = Arc::new(A2ARelay::new(store));
    let router = create_router(relay.clone());

    let addr = config::EnvConfig::from_env()
        .map(|c| c.infra.a2a_relay_addr)
        .unwrap_or_else(|_| "127.0.0.1:3000".to_string());
    let listener = tokio::net::TcpListener::bind(&addr).await?;
    info!(
        addr = %addr,
        "A2A relay HTTP server starting"
    );

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "A2A relay HTTP server error");
        }
    });

    Ok(relay)
}
