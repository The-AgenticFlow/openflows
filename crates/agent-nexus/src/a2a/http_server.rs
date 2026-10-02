// crates/agent-nexus/src/a2a/http_server.rs
//! JSON-RPC HTTP server for A2A relay. Implements the standard A2A methods:
//! - message/send (submit verify request)
//! - message/stream (subscribe to task events via SSE)
//! - tasks/get (retrieve task details)
//! - tasks/cancel (cancel a running task)
//! - tasks/resubscribe (resume after disconnect)

use super::routing::{A2ARelay, TaskState};
use super::verify_handler::{submit_verify_request, InvalidVerifyRequest};
use a2a_protocol::{VerifyProgressEvent, VerifyRequest, VerifyResult};
use anyhow::Context;
use axum::{
    extract::{Query, State},
    http::StatusCode,
    response::{
        sse::{Event, Sse},
        IntoResponse, Response,
    },
    routing::{get, post},
    Json, Router,
};
use futures::stream;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::convert::Infallible;
use std::sync::Arc;
use tokio_stream::wrappers::BroadcastStream;
use tokio_stream::StreamExt;
use tracing::{debug, warn};

/// JSON-RPC 2.0 request envelope (simplified for A2A).
#[derive(Debug, Deserialize)]
pub struct JsonRpcRequest {
    #[allow(dead_code)]
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
    #[serde(default)]
    pub id: Value,
}

/// JSON-RPC 2.0 response envelope.
#[derive(Debug, Serialize)]
pub struct JsonRpcResponse {
    pub jsonrpc: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<JsonRpcError>,
    pub id: Value,
}

impl IntoResponse for JsonRpcResponse {
    fn into_response(self) -> Response {
        Json(self).into_response()
    }
}

/// JSON-RPC error object.
#[derive(Debug, Serialize)]
pub struct JsonRpcError {
    pub code: i32,
    pub message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub data: Option<Value>,
}

/// A2A HTTP server state.
#[derive(Clone)]
pub struct A2AServerState {
    pub relay: Arc<A2ARelay>,
}

/// Create the A2A HTTP server router.
pub fn create_router(relay: Arc<A2ARelay>) -> Router {
    let state = A2AServerState { relay };

    Router::new()
        .route("/rpc", post(handle_rpc))
        .route("/", get(handle_stream))
        .route("/health", get(handle_health))
        .route("/.well-known/agent-card.json", get(handle_agent_card))
        .with_state(state)
}

/// Health check endpoint.
async fn handle_health() -> &'static str {
    "A2A relay healthy"
}

/// Agent Card endpoint. Describes the relay's capabilities (not the executors).
async fn handle_agent_card() -> Json<Value> {
    Json(json!({
        "name": "OpenFlows A2A Relay",
        "version": "1.0",
        "description": "Central A2A relay for Sentinel↔Forge delegated verification (issue #143)",
        "capabilities": {
            "verify": {
                "description": "Verify task execution in a temporary FORGE checkout with command policy checks",
                "task_type": "verify",
                "methods": [
                    "message/send",
                    "message/stream",
                    "tasks/get",
                    "tasks/cancel",
                    "tasks/resubscribe"
                ]
            }
        },
        "endpoints": {
            "rpc": "/rpc",
            "stream": "/",
            "health": "/health"
        },
        "api_version": "A2A 1.0",
        "source": "https://github.com/The-AgenticFlow/openflows/issues/143"
    }))
}

/// JSON-RPC POST handler. Processes message/send and other A2A methods.
async fn handle_rpc(
    State(state): State<A2AServerState>,
    Json(req): Json<JsonRpcRequest>,
) -> Response {
    debug!(method = %req.method, "RPC request");

    let result = match req.method.as_str() {
        "message/send" => handle_message_send(&state, &req.params).await,
        "tasks/get" => handle_tasks_get(&state, &req.params).await,
        "tasks/claim" => handle_tasks_claim(&state, &req.params).await,
        "tasks/complete" => handle_tasks_complete(&state, &req.params).await,
        "tasks/cancel" => handle_tasks_cancel(&state, &req.params).await,
        "tasks/resubscribe" => handle_tasks_resubscribe(&state, &req.params).await,
        "tasks/push_progress" => handle_tasks_progress(&state, &req.params).await,
        _ => {
            return JsonRpcResponse {
                jsonrpc: "2.0".into(),
                result: None,
                error: Some(JsonRpcError {
                    code: -32601,
                    message: format!("Method not found: {}", req.method),
                    data: None,
                }),
                id: req.id,
            }
            .into_response();
        }
    };

    match result {
        Ok(result) => JsonRpcResponse {
            jsonrpc: "2.0".into(),
            result: Some(result),
            error: None,
            id: req.id,
        }
        .into_response(),
        Err(e) => {
            warn!(error = %e, method = %req.method, request_id = %req.id, "RPC handler error");
            JsonRpcResponse {
                jsonrpc: "2.0".into(),
                result: None,
                error: Some(JsonRpcError {
                    code: if e.is::<InvalidVerifyRequest>() {
                        -32602
                    } else {
                        -32603
                    },
                    message: if let Some(invalid) = e.downcast_ref::<InvalidVerifyRequest>() {
                        invalid.to_string()
                    } else {
                        "Internal error; inspect NEXUS relay logs for this request".into()
                    },
                    data: None,
                }),
                id: req.id,
            }
            .into_response()
        }
    }
}

/// message/send: Submit a verify request (Sentinel → Nexus)
///
/// `params` is the `verify` task payload: `{ "pair_id", "kind", "cwd",
/// "argv", "timeout_secs", "env_allowlist", "expect" }`. The requester's
/// identity is authenticated out-of-band (workspace token); the relay
/// enforces that a Sentinel may only submit for its own `pair_id`.
async fn handle_message_send(state: &A2AServerState, params: &Value) -> anyhow::Result<Value> {
    // The message params nest the verify task under the otherMembers tasks.
    // Per the A2A wire shape used by the harness, params may be the
    // VerifyRequest directly, or wrapped as `{ "message": { "task": ... } }`.
    let raw = params
        .get("message")
        .and_then(|m| m.get("task"))
        .unwrap_or(params);

    let req: VerifyRequest = serde_json::from_value(raw.clone())
        .map_err(|e| InvalidVerifyRequest(format!("Invalid VerifyRequest: {e}")))?;

    // AuthZ (v1): the Docker network boundary is the trust model for this
    // deployment.  pair_id is caller-supplied (self-declared) — a malicious
    // workspace on the shared network could impersonate another pair.  The
    // executor role check on claim (tasks/claim rejects non-Forge roles) and
    // the unguessable UUIDv7 task IDs provide layers of defence, but these
    // are NOT cryptographic guarantees.
    //
    // TODO(v2): bind pair-scope to a workspace identity token (plan task 2)
    // so the relay can verify ownership without trusting self-declared values.
    let requester_pair_id = req.pair_id.clone();

    let task_id = submit_verify_request(&state.relay, &req, &requester_pair_id).await?;

    Ok(json!({"task_id": task_id, "status": "pending"}))
}

/// tasks/get: Retrieve the state of a task (Sentinel polling, Forge status).
///
/// `params: { "task_id": "..." }` → returns the current lifecycle state and,
/// once completed, the terminal result the executor mirrored to Redis.
async fn handle_tasks_get(state: &A2AServerState, params: &Value) -> anyhow::Result<Value> {
    let task_id = params
        .get("task_id")
        .and_then(|t| t.as_str())
        .context("tasks/get requires string task_id")?;

    let entry = state
        .relay
        .get_task(task_id)
        .await
        .context("task not found")?;

    let state_str = match entry.state {
        TaskState::Pending => "pending",
        TaskState::Running => "running",
        TaskState::Completed => "completed",
        TaskState::Cancelled => "cancelled",
    };

    let mut value = json!({
        "task_id": entry.task_id,
        "pair_id": entry.request.pair_id,
        "status": state_str,
    });
    if let Some(result) = entry.result {
        value["result"] = serde_json::to_value(result)?;
    }
    Ok(value)
}

/// tasks/claim: Forge executor claims the next pending task for its pair.
///
/// `params: { "pair_id": "T-048", "role": "forge" }` → returns the task
/// (request included) or null when no pending task exists for the pair.
async fn handle_tasks_claim(state: &A2AServerState, params: &Value) -> anyhow::Result<Value> {
    let pair_id = params
        .get("pair_id")
        .and_then(|p| p.as_str())
        .context("tasks/claim requires string pair_id")?;
    let role = params
        .get("role")
        .and_then(|r| r.as_str())
        .context("tasks/claim requires string role")?;

    // Only the Forge executor role may claim verify tasks.
    if !role.eq_ignore_ascii_case("forge") {
        warn!(role, "Non-forge role attempted to claim a verify task");
        return Err(anyhow::anyhow!(
            "tasks/claim is only available to the forge executor role"
        ));
    }

    anyhow::ensure!(
        params.get("checkout_version").and_then(Value::as_u64) == Some(1),
        "Executor upgrade required: temporary verification checkout version 1 is mandatory"
    );
    match state.relay.claim_next_task(pair_id).await? {
        Some(entry) => Ok(json!({
            "task_id": entry.task_id,
            "request": serde_json::to_value(entry.request)?,
        })),
        None => Ok(json!(null)),
    }
}

/// tasks/complete: Forge executor submits a terminal result for a task.
///
/// `params: { "task_id", "pair_id", "result": { VerifyResult } }` → the relay
/// marks the task completed and mirrors the result to Redis before returning.
async fn handle_tasks_complete(state: &A2AServerState, params: &Value) -> anyhow::Result<Value> {
    let task_id = params
        .get("task_id")
        .and_then(|t| t.as_str())
        .context("tasks/complete requires string task_id")?;
    let pair_id = params
        .get("pair_id")
        .and_then(|p| p.as_str())
        .context("tasks/complete requires string pair_id")?;

    let result: VerifyResult = params
        .get("result")
        .cloned()
        .map(serde_json::from_value)
        .transpose()
        .context("tasks/complete requires a valid VerifyResult result")?
        .context("tasks/complete requires a result")?;

    // Guard: the submitted task_id must match the one the executor built the
    // result from, so a mistaken/stale result can't land on another task.
    if result.task_id != task_id {
        return Err(anyhow::anyhow!(
            "tasks/complete task_id mismatch: result carries {}, request says {}",
            result.task_id,
            task_id
        ));
    }

    // Guard (v1 best-effort): verify the caller's pair_id matches the owning
    // task's pair_id so a workspace cannot complete another pair's task.
    if let Some(entry) = state.relay.get_task(task_id).await {
        if entry.request.pair_id != pair_id {
            return Err(anyhow::anyhow!(
                "tasks/complete pair_id mismatch: task belongs to {} but caller claims {}",
                entry.request.pair_id,
                pair_id
            ));
        }
    }

    state.relay.complete_task(task_id, result).await?;
    Ok(json!({"task_id": task_id, "status": "completed"}))
}

/// tasks/cancel: Cancel a running task
///
/// Sets the cancel flag on the task. For in-process executors (the current
/// topology), the `verify serve` daemon checks this flag via the cancel token
/// and kills the child process group when the flag is set.
async fn handle_tasks_cancel(state: &A2AServerState, params: &Value) -> anyhow::Result<Value> {
    let task_id = params
        .get("task_id")
        .and_then(|t| t.as_str())
        .context("tasks/cancel requires string task_id")?;
    let pair_id = params
        .get("pair_id")
        .and_then(|p| p.as_str())
        .context("tasks/cancel requires string pair_id")?;

    // Guard (best-effort, v1): the pair_id here is self-declared by the
    // caller — a workspace that knows another pair's task ID can supply
    // the owning pair_id to bypass this check.  The real defence is the
    // Docker network boundary + unguessable UUIDv7 task IDs.
    //
    // TODO(v2): replace with workspace-identity-backed ownership verification
    // when the relay gains signed workspace credentials (plan task 2).
    if let Some(entry) = state.relay.get_task(task_id).await {
        if entry.request.pair_id != pair_id {
            return Err(anyhow::anyhow!(
                "tasks/cancel pair_id mismatch: task belongs to {} but caller claims {}",
                entry.request.pair_id,
                pair_id
            ));
        }
    }

    // Transition the task to Cancelled state with a synthetic result.
    // The executor's cancel poller observes "cancelled" and sets the
    // local cancel token to kill the child process. We intentionally do
    // NOT call complete_task here — that would transition to Completed
    // before the poller can detect the cancellation.
    if let Some(entry) = state.relay.get_task(task_id).await {
        if entry.state == TaskState::Running || entry.state == TaskState::Pending {
            let cancelled_result = a2a_protocol::VerifyResult {
                head_sha: None,
                task_id: task_id.to_string(),
                exit_code: None,
                timed_out: false,
                duration_ms: 0,
                stdout_ref: format!("audit:a2a:{}:stdout", task_id),
                stderr_ref: format!("audit:a2a:{}:stderr", task_id),
                artifacts: vec![],
                executor: a2a_protocol::ExecutorInfo {
                    role: "forge".to_string(),
                    workspace: format!("unknown-{}", entry.request.pair_id),
                },
            };
            let _ = state.relay.cancel_task(task_id, cancelled_result).await;
        }
    }

    Ok(json!({
        "task_id": task_id,
        "cancelled": true
    }))
}

/// tasks/resubscribe: Resume SSE subscription after disconnect
///
/// `params: { "task_id": "...", "last_event_id": 42 }` → returns buffered
/// events since that sequence number. The caller then reconnects to the SSE
/// endpoint to receive live events.
async fn handle_tasks_resubscribe(state: &A2AServerState, params: &Value) -> anyhow::Result<Value> {
    let task_id = params
        .get("task_id")
        .and_then(|t| t.as_str())
        .context("tasks/resubscribe requires string task_id")?;

    let last_event_id = params
        .get("last_event_id")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    let events = state
        .relay
        .replay_events_since(task_id, last_event_id)
        .await;
    let current_seq = state.relay.current_event_seq(task_id).await;

    Ok(json!({
        "task_id": task_id,
        "events": events,
        "current_seq": current_seq,
        "event_count": events.len()
    }))
}

/// tasks/push_progress: Forge pushes a progress chunk to the relay
///
/// `params: { "task_id": "...", "stream": "stdout", "chunk": "..." }`
/// The relay buffers the event and broadcasts it to SSE subscribers.
async fn handle_tasks_progress(state: &A2AServerState, params: &Value) -> anyhow::Result<Value> {
    let task_id = params
        .get("task_id")
        .and_then(|t| t.as_str())
        .context("tasks/push_progress requires string task_id")?;

    let event: VerifyProgressEvent = serde_json::from_value(params.clone())
        .context("tasks/push_progress requires valid VerifyProgressEvent params")?;

    let buffered = state.relay.push_progress_event(task_id, event).await?;

    Ok(json!({
        "task_id": task_id,
        "seq": buffered.seq
    }))
}

/// Server-Sent Events endpoint for progress streaming.
///
/// Query params: `?task_id=<uuid>`
/// Returns an SSE stream of `VerifyProgressEvent` chunks as the task executes.
/// On connect, replays any buffered events for the task, then streams live
/// events until the task completes or the client disconnects.
async fn handle_stream(
    State(state): State<A2AServerState>,
    Query(params): Query<std::collections::HashMap<String, String>>,
) -> Response {
    let task_id = match params.get("task_id") {
        Some(id) if !id.is_empty() => id.clone(),
        _ => {
            return (
                StatusCode::BAD_REQUEST,
                "Missing or empty task_id query parameter",
            )
                .into_response();
        }
    };

    // 1. Replay buffered events
    let buffered = state.relay.replay_events_since(&task_id, 0).await;

    // 2. Subscribe to live broadcast
    let rx = state.relay.subscribe_to_task(&task_id).await;
    let broadcast_stream = BroadcastStream::new(rx).filter_map(Result::ok);

    // 3. Chain: buffered events first, then live broadcast
    let replay_stream = stream::iter(buffered);
    let combined = replay_stream.chain(broadcast_stream);

    // 4. Map to SSE events
    let sse_stream = combined.map(|event| {
        let data = serde_json::to_string(&event.event).unwrap_or_default();
        Ok::<_, Infallible>(
            Event::default()
                .data(data)
                .event("progress")
                .id(event.seq.to_string()),
        )
    });

    Sse::new(sse_stream).into_response()
}

#[cfg(test)]
mod regression_tests {
    use super::*;

    async fn submit(argv: Value) -> Value {
        let state = A2AServerState {
            relay: Arc::new(A2ARelay::new(Arc::new(
                pocketflow_core::SharedStore::new_in_memory(),
            ))),
        };
        let response = handle_rpc(
            State(state),
            Json(JsonRpcRequest {
                jsonrpc: "2.0".into(),
                method: "message/send".into(),
                id: json!(1),
                params: json!({"pair_id":"T-066", "kind":"command", "cwd":"repo",
                "argv":argv, "timeout_secs":30}),
            }),
        )
        .await;
        let bytes = axum::body::to_bytes(response.into_body(), 65536)
            .await
            .unwrap();
        serde_json::from_slice(&bytes).unwrap()
    }

    #[tokio::test]
    async fn rejected_command_explains_policy_instead_of_internal_error() {
        let body = submit(json!(["rm", "-rf", "/"])).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("policy denied"));
    }

    #[tokio::test]
    async fn executors_without_temporary_checkout_cannot_claim_tasks() {
        let state = A2AServerState {
            relay: Arc::new(A2ARelay::new(Arc::new(
                pocketflow_core::SharedStore::new_in_memory(),
            ))),
        };
        assert!(
            handle_tasks_claim(&state, &json!({"role":"forge", "pair_id":"T-066"}))
                .await
                .is_err()
        );
        assert!(handle_tasks_claim(
            &state,
            &json!({"role":"forge", "pair_id":"T-066", "sandbox_version":1})
        )
        .await
        .is_err());
        assert!(handle_tasks_claim(
            &state,
            &json!({"role":"forge", "pair_id":"T-066", "checkout_version":1})
        )
        .await
        .unwrap()
        .is_null());
    }

    #[tokio::test]
    async fn quoted_command_reports_tokenization_error() {
        let body = submit(json!(["cargo test --workspace --all-features"])).await;
        assert_eq!(body["error"]["code"], -32602);
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("single argument"));
        assert!(body["error"]["message"]
            .as_str()
            .unwrap()
            .contains("-- cargo test"));
    }

    #[tokio::test]
    async fn allowed_request_is_queued_without_registered_executor() {
        let body = submit(json!(["cargo", "test"])).await;
        assert!(body.get("error").is_none(), "{body}");
        assert_eq!(body["result"]["status"], "pending");
        assert!(body["result"]["task_id"].is_string());
    }
}
