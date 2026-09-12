// crates/agent-nexus/src/hooks/server.rs
//! Axum webhook consumer for Coder's experimental `agent-lifecycle-hooks`.
//!
//! Coder `chatd` POSTs a JWT-signed lifecycle event to
//! `CODER_CHAT_HOOK_URL`. This module hosts that endpoint inside the
//! OpenFlows Controller (the same process that hosts the A2A relay). It:
//!
//!   1. Verifies the HS256 JWT (signature, iss, aud, exp, jti, body_sha256).
//!   2. Decodes the lifecycle event.
//!   3. Observes it: logs centrally and persists a durable, tenant-namespaced
//!      audit trail in Redis (mirroring the A2A audit model).
//!   4. Applies policy for mutable events (`user_prompt_submit`,
//!      `pre_tool_use`) — today a passthrough; the seam is where OpenFlows'
//!      in-workspace PreToolUse/PreWrite policy could be centralized.
//!
//! This is the OpenFlows-side analogue of Coder's `scripts/agenthooks-server`
//! reference consumer. It lets us turn the experiment on and *observe behaviour*
//! before we decide how much policy to centralize.

use super::bootstrap::HookBootstrapContext;
use super::jwt::verify_hook_jwt;
use super::types::{HookDecision, HookEvent, HookPayload};
use anyhow::{anyhow, Context, Result};
use config::env::CoderHooksConfig;
use axum::{
    extract::State,
    http::{header, StatusCode},
    response::{IntoResponse, Response},
    routing::post,
    Json, Router,
};
use pocketflow_core::{HookKickPublisher, SharedStore};
use serde_json::{json, Value};
use std::sync::Arc;
use tracing::{debug, info, warn};

/// Shared state for the hook consumer.
#[derive(Clone)]
pub struct HookServerState {
    pub store: Arc<SharedStore>,
    pub config: CoderHooksConfig,
    /// Publisher for the reactive kick channel (Slice B). `None` when the kick
    /// feature is disabled or the channel could not be built (fail-open).
    pub publisher: Option<HookKickPublisher>,
    /// Resolved persona/skills/commands paths for `session_start` bootstrap
    /// (Slice A). `None` when bootstrap is disabled or not provided.
    pub bootstrap: Option<HookBootstrapContext>,
}

/// Build the router serving the webhook endpoint.
pub fn create_router(
    store: Arc<SharedStore>,
    config: CoderHooksConfig,
    publisher: Option<HookKickPublisher>,
    bootstrap: Option<HookBootstrapContext>,
) -> Router {
    Router::new()
        .route("/experimental/hooks/chat", post(handle_hook))
        .route("/experimental/hooks/health", axum::routing::get(handle_health))
        .with_state(HookServerState {
            store,
            config,
            publisher,
            bootstrap,
        })
}

async fn handle_health() -> &'static str {
    "OpenFlows lifecycle hook consumer healthy"
}

/// Handle a single lifecycle hook dispatch from Coder.
async fn handle_hook(
    State(state): State<HookServerState>,
    headers: axum::http::HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    let hook_url = match state.config.hook_public_url() {
        Some(u) => u,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "hook consumer not enabled (bind address has no usable port)",
            )
                .into_response();
        }
    };
    let secret = match state.config.chat_hook_secret.clone() {
        Some(s) => s,
        None => {
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                "hook consumer misconfigured (CODER_CHAT_HOOK_SECRET unset)",
            )
                .into_response();
        }
    };

    // Parse the payload first so we can bind jti for the signature check.
    let payload: HookPayload = match serde_json::from_slice(&body) {
        Ok(p) => p,
        Err(e) => {
            warn!(error = %e, "Hook consumer: malformed JSON body");
            return (StatusCode::BAD_REQUEST, "invalid JSON body").into_response();
        }
    };
    let dispatch_id = payload.meta.dispatch_id.clone();
    let chat_id = payload.meta.chat_id.clone();
    let event_str = match serde_json::to_string(&payload.event) {
        Ok(s) => s.trim_matches('"').to_string(),
        Err(_) => "unknown".to_string(),
    };

    // Build the audit key BEFORE verifying so a failed signature is still
    // observable (we only persist after verification, below).
    debug!(
        dispatch_id = %dispatch_id,
        event = ?payload.event,
        "Hook consumer: received dispatch"
    );

    // Verify the JWT.
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok());
    if let Err(e) = verify_hook_jwt(
        auth,
        &secret,
        &hook_url,
        &dispatch_id,
        &chat_id,
        &event_str,
        &body,
    ) {
        warn!(error = %e, "Hook consumer: JWT verification failed");
        // Fail closed: coder treats a failed consumer as chat error state.
        return (StatusCode::UNAUTHORIZED, format!("{e:#}")).into_response();
    }

    info!(
        dispatch_id = %dispatch_id,
        chat_id = %chat_id,
        event = ?payload.event,
        client = ?payload.meta.client,
        "OpenFlows observed Coder lifecycle hook event"
    );

    // Persist a durable audit trail (best-effort; never fail the dispatch).
    let _ = persist_audit(&state.store, &payload).await;

    // Apply experimental hook-driven behaviour (slices A–D), then serialize the
    // decision into Coder's response schema.
    //
    // A policy decision (deny/rewrite/model_context) is carried in the JSON body
    // and returned with a 200 so Coder reads it as a deliberate decision, NOT a
    // dispatch failure. Non-2xx is reserved for real dispatch failures (bad JWT /
    // malformed body) earlier in this handler, which fail closed.
    let body = route_event(&state, &payload).await;
    (StatusCode::OK, Json(body)).into_response()
}

/// Route a single verified dispatch to the experimental slice behaviour and
/// return the Coder response body. Falls back to the generic `apply_policy`
/// decision for everything not covered by a slice.
async fn route_event(state: &HookServerState, payload: &HookPayload) -> serde_json::Value {
    let chat_id = payload.meta.chat_id.clone();
    let dispatch_id = payload.meta.dispatch_id.clone();

    match payload.event {
        HookEvent::SessionStart => {
            // Slice A: bootstrap context via `model_context` (persona + skills +
            // command paths + resume state). If bootstrap context is unavailable
            // or the chat isn't resolvable, fall back to observation-only.
            if let Some(ctx) = &state.bootstrap {
                if let Some(context) =
                    super::bootstrap::build_bootstrap_context(&state.store, &chat_id, &payload.data, ctx).await
                {
                    return json!({ "model_context": context });
                }
            }
            decision_to_response(&HookDecision::observe())
        }

        HookEvent::PostToolUse => {
            // Slice B: reactive wake when a harness state mutation lands, so the
            // Controller can re-run its reconciliation pass without waiting the
            // full poll interval (e.g. a sentinel verdict → resume paused forge).
            if let Some(publisher) = &state.publisher {
                super::kick::maybe_publish_kick(
                    publisher,
                    &state.store,
                    &chat_id,
                    &dispatch_id,
                    HookEvent::PostToolUse,
                    &payload.data,
                )
                .await;
            }
            decision_to_response(&HookDecision::observe())
        }

        HookEvent::Stop => {
            // Slice D: status-aware, authorized stop monitoring. Classify why the
            // agent wants to stop against durable state; persist the record and,
            // for a planned handoff, kick Nexus to reconcile the paused agent.
            let (classification, detail) = super::stop::classify_stop(&state.store, &chat_id).await;
            if let (Some(ticket), Some(publisher)) =
                (detail.get("ticket_id").and_then(|v| v.as_str()), &state.publisher)
            {
                let _ = state
                    .store
                    .set(
                        &format!("ticket:{ticket}:hooks:stop"),
                        super::stop::stop_audit_record(&classification, &detail),
                    )
                    .await;
                if classification == super::stop::StopClassification::PlannedHandoff {
                    let kick = pocketflow_core::HookKick {
                        chat_id: chat_id.clone(),
                        dispatch_id: dispatch_id.clone(),
                        event: "stop".to_string(),
                        role: detail.get("role").and_then(|v| v.as_str()).unwrap_or_default().to_string(),
                        ticket_id: ticket.to_string(),
                        hint: "stop_planned_handoff".to_string(),
                        data: detail.clone(),
                    };
                    publisher.publish(&kick).await;
                }
            }
            decision_to_response(&HookDecision::observe())
        }

        HookEvent::PreToolUse => {
            // Generic policy (rm -rf, force-push, redis-cli, workspace escape)
            // layered with Slice C's stateful, phase-aware write guard.
            let base = apply_policy(payload);
            let role = super::context::resolve_chat(&state.store, &chat_id).await.role.unwrap_or_default();
            let tool_name = payload.data.get("tool_name").and_then(|v| v.as_str()).unwrap_or_default();
            let input = payload.data.get("tool_input").unwrap_or(&Value::Null);
            decision_to_response(
                &super::guard::phase_guard(&state.store, &chat_id, &role, tool_name, input, base).await,
            )
        }

        HookEvent::UserPromptSubmit | HookEvent::PreCompact | HookEvent::PostCompact => {
            decision_to_response(&apply_policy(payload))
        }
    }
}

/// Convert the internal [`HookDecision`] into Coder's response-body schema.
/// A denial carries `permission.decision=deny`; a rewrite carries
/// `permission.decision=allow` with `permission.input_override`; an
/// observation yields an empty JSON object (equivalent to an empty body).
fn decision_to_response(decision: &HookDecision) -> serde_json::Value {
    if !decision.deny && decision.rewrite.is_none() {
        // No-op: Coder accepts an empty JSON object.
        return serde_json::json!({});
    }
    if decision.deny {
        let mut permission = serde_json::json!({ "decision": "deny" });
        if let Some(reason) = &decision.reason {
            permission["user_message"] = serde_json::Value::String(reason.clone());
        }
        return serde_json::json!({ "permission": permission });
    }
    // allow + input_override (rewrite). input_override must carry the full,
    // canonical replacement for the tool input or prompt.
    if let Some(rewrite) = &decision.rewrite {
        return serde_json::json!({
            "permission": {
                "decision": "allow",
                "input_override": rewrite
            }
        });
    }
    serde_json::json!({})
}

/// Centralized policy seam. Mutable events can be denied or rewritten here.
///
/// This is the server-side twin of the in-workspace `pre_bash_guard.sh` /
/// `pre_write_check.sh` scripts, but enforced in the trusted control plane so
/// a workspace cannot bypass it. Rules:
///   - Any non-mutable event → observe (no decision).
///   - `pre_tool_use` → inspect `tool_name` + `tool_input`, then either allow
///     unchanged, deny, or return an `input_override` (rewrite).
///   - `user_prompt_submit` → deny rewriting that would drop attachments
///     (see D4 in the feedback doc); by default observe.
///
/// Deny/override logic mirrors forge `pre_bash_guard.sh`: destructive shell,
/// force-push to default branch, direct Redis, control-plane mutation.
fn apply_policy(payload: &HookPayload) -> HookDecision {
    match payload.event {
        HookEvent::PreToolUse => decide_pre_tool_use(&payload.data),
        HookEvent::UserPromptSubmit => decide_user_prompt(&payload.data),
        _ => HookDecision::observe(),
    }
}

/// Decision for a `pre_tool_use` dispatch.
fn decide_pre_tool_use(data: &Value) -> HookDecision {
    let tool_name = data
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_lowercase();
    let tool_input = data.get("tool_input").unwrap_or(&Value::Null);

    // Only gate tools that carry a command/filename argument.
    match tool_name.as_str() {
        "bash" | "sh" | "shell" | "execute" | "exec" => {
            let cmd = extract_command(tool_input).unwrap_or_default();
            classify_command(&cmd)
        }
        "write" | "edit" | "create" | "patch" => {
            // Prevent writes outside the workspace root.
            let path = tool_input
                .get("path")
                .or_else(|| tool_input.get("file_path"))
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if is_outside_workspace(&path) {
                return HookDecision::deny(format!(
                    "openflows policy: write outside workspace (`{path}`) is blocked"
                ));
            }
            // Override example: force benign edit tool flags (rewrite demo).
            HookDecision::observe()
        }
        _ => {
            // Unknown / MCP / dynamic tool: no built-in duplicate-key precheck
            // exists server-side (D4), so allow and rely on the workspace.
            debug!(tool = tool_name, "Hook consumer: ungated tool passed through");
            HookDecision::observe()
        }
    }
}

/// Decision for a `user_prompt_submit` dispatch. Today we only guard against
/// an override that would silently drop file attachments; otherwise observe.
fn decide_user_prompt(data: &Value) -> HookDecision {
    let parts = data.get("parts");
    if let Some(parts) = parts {
        if let Some(arr) = parts.as_array() {
            let non_text = arr
                .iter()
                .filter(|p| p.get("type").and_then(|t| t.as_str()) != Some("text"))
                .count();
            if non_text > 0 {
                // If we were to override the prompt, we must NOT drop these.
                // We observe (don't override) so attachments survive.
                debug!(
                    non_text_parts = non_text,
                    "Hook consumer: prompt has attachments; not overriding to avoid dropping them"
                );
                return HookDecision::observe();
            }
        }
    }
    HookDecision::observe()
}

/// Pull the shell command out of a tool_input, tolerating the common shapes
/// (`command`, `argv`, or plain string payload).
fn extract_command(tool_input: &Value) -> Option<String> {
    if let Some(c) = tool_input.get("command").and_then(|v| v.as_str()) {
        return Some(c.to_string());
    }
    if let Some(argv) = tool_input.get("argv").and_then(|v| v.as_array()) {
        return Some(
            argv.iter()
                .filter_map(|v| v.as_str())
                .collect::<Vec<_>>()
                .join(" "),
        );
    }
    if let Some(s) = tool_input.as_str() {
        return Some(s.to_string());
    }
    None
}

/// Classify a shell command into a decision. Ported from forge
/// `pre_bash_guard.sh`.
fn classify_command(cmd: &str) -> HookDecision {
    let deny = |reason: String| HookDecision::deny(reason);

    if cmd.contains("rm -rf /") || cmd.contains("rm -rf /*") {
        return deny("recursive delete of filesystem root".into());
    }
    if (cmd.contains("git push") && cmd.contains("--force") && cmd.contains("main"))
        || (cmd.contains("git push") && (cmd.contains("-f") || cmd.contains("--force")) && cmd.contains("master"))
    {
        return deny("force-push to default branch".into());
    }
    if cmd.contains("redis-cli") {
        return deny("direct Redis access — use openflows-harness for all coordination".into());
    }
    if cmd.contains("coder templates") || cmd.contains("coder delete") || cmd.contains("coder server") {
        return deny("control-plane mutation from a worker workspace".into());
    }

    // Override example: wrap risky-but-allowed git pushes to avoid interactive
    // prompts hanging the loop (rewrite demo — shows the mechanism).
    if cmd.trim_start().starts_with("git push") {
        let override_cmd = format!(
            "GIT_TERMINAL_PROMPT=0 git -c core.askpass=true {}",
            cmd.trim()
        );
        return HookDecision {
            deny: false,
            reason: None,
            rewrite: Some(serde_json::json!({ "command": override_cmd })),
        };
    }

    HookDecision::observe()
}

/// True when the given path is absolute and points outside the workspace.
fn is_outside_workspace(path: &str) -> bool {
    let trimmed = path.trim_start_matches('~').trim_start_matches("./");
    // Absolute paths referencing system roots are treated as outside unless
    // they clearly live under a /home/coder/workspace or /workspace root.
    if trimmed.starts_with('/') {
        let outside = [
            "/home/coder/workspace",
            "/home/coder",
            "/workspace",
            "/home/",
            ".",
        ]
        .iter()
        .all(|root| !trimmed.starts_with(root));
        return outside;
    }
    false
}

/// Write a tenant-namespaced, durable audit record for a hook event.
async fn persist_audit(store: &SharedStore, payload: &HookPayload) -> Result<()> {
    let mut events: Vec<Value> = store.get_typed("_hook_events_tail").await.unwrap_or_default();
    events.push(json!({
        "dispatch_id": payload.meta.dispatch_id,
        "event": payload.event,
        "chat_id": payload.meta.chat_id,
        "owner_id": payload.meta.owner_id,
        "workspace_id": payload.meta.workspace_id,
        "client": payload.meta.client,
        "data": payload.data,
        "ts": chrono::Utc::now().timestamp(),
    }));
    if events.len() > 500 {
        let drop = events.len() - 500;
        events.drain(0..drop);
    }
    store.set("_hook_events_tail", serde_json::to_value(events)?).await;
    Ok(())
}

/// Start the hook consumer HTTP server as a background task.
///
/// Only binds when the experiment + config indicate it is enabled. Returns
/// `Ok(None)` when disabled so the controller startup stays explicit.
pub async fn start_lifecycle_hook_server(
    store: Arc<SharedStore>,
    config: CoderHooksConfig,
    publisher: Option<HookKickPublisher>,
    bootstrap: Option<HookBootstrapContext>,
) -> Result<Option<()>> {
    if !config.enabled() {
        info!(
            "Coder agent lifecycle hooks experiment not enabled; hook consumer not started \
             (set CODER_EXPERIMENTS=agent-lifecycle-hooks)"
        );
        return Ok(None);
    }
    if config.chat_hook_secret.as_deref().map(str::len).unwrap_or(0) < 32 {
        return Err(anyhow!(
            "CODER_CHAT_HOOK_SECRET must be at least 32 bytes to verify HS256 hook JWTs"
        ));
    }

    let router = create_router(store, config.clone(), publisher, bootstrap);
    let addr = config.hook_addr.clone();
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("failed to bind hook consumer on {addr}"))?;
    info!(
        addr = %addr,
        hook_url = %config.hook_public_url().unwrap_or_default(),
        "OpenFlows lifecycle hook consumer starting"
    );

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, router).await {
            tracing::error!(error = %e, "Lifecycle hook consumer HTTP server error");
        }
    });

    Ok(Some(()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn tool(tool_name: &str, input: Value) -> Value {
        json!({ "tool_name": tool_name, "tool_input": input })
    }

    #[test]
    fn allows_safe_command() {
        let data = tool("Bash", json!({ "command": "openflows-harness status set building" }));
        let d = decide_pre_tool_use(&data);
        assert!(!d.deny);
        assert!(d.rewrite.is_none());
    }

    #[test]
    fn denies_destructive_command() {
        let data = tool("Bash", json!({ "command": "rm -rf /" }));
        let d = decide_pre_tool_use(&data);
        assert!(d.deny);
    }

    #[test]
    fn denies_force_push_to_main() {
        let data = tool("Bash", json!({ "command": "git push --force origin main" }));
        let d = decide_pre_tool_use(&data);
        assert!(d.deny);
    }

    #[test]
    fn denies_redis_cli() {
        let data = tool("Bash", json!({ "command": "redis-cli flushall" }));
        let d = decide_pre_tool_use(&data);
        assert!(d.deny);
    }

    #[test]
    fn rewrite_wraps_git_push() {
        let data = tool("Bash", json!({ "command": "git push origin feature/x" }));
        let d = decide_pre_tool_use(&data);
        assert!(!d.deny);
        let rewrite = d.rewrite.expect("git push should be rewritten");
        let cmd = rewrite["command"].as_str().unwrap();
        assert!(cmd.starts_with("GIT_TERMINAL_PROMPT=0"));
    }

    #[test]
    fn denies_workspace_escape_write() {
        let data = tool("Write", json!({ "path": "/etc/passwd" }));
        let d = decide_pre_tool_use(&data);
        assert!(d.deny);
    }

    #[test]
    fn allows_workspace_write() {
        let data = tool("Write", json!({ "path": "/home/coder/workspace/src/main.rs" }));
        let d = decide_pre_tool_use(&data);
        assert!(!d.deny);
    }

    #[test]
    fn deny_serializes_to_coder_schema() {
        let d = HookDecision::deny("blocked");
        let v = decision_to_response(&d);
        assert_eq!(v["permission"]["decision"], "deny");
        assert_eq!(v["permission"]["user_message"], "blocked");
    }

    #[test]
    fn rewrite_serializes_as_input_override() {
        let d = HookDecision {
            deny: false,
            reason: None,
            rewrite: Some(json!({ "command": "echo hi" })),
        };
        let v = decision_to_response(&d);
        assert_eq!(v["permission"]["decision"], "allow");
        assert_eq!(v["permission"]["input_override"]["command"], "echo hi");
    }

    #[test]
    fn observe_serializes_empty() {
        let v = decision_to_response(&HookDecision::observe());
        assert!(v.as_object().map(|o| o.is_empty()).unwrap_or(false));
    }
}
