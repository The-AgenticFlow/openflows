// crates/agent-nexus/src/a2a/verify_handler.rs
//! Handler for `verify` A2A task type. Validates requests, routes to
//! executor, manages result dedup and persistence.

use super::routing::A2ARelay;
use a2a_protocol::VerifyRequest;
use anyhow::Result;
use serde_json::json;
use tracing::warn;

/// Caller-correctable verification input error, safe to return over JSON-RPC.
#[derive(Debug)]
pub struct InvalidVerifyRequest(pub String);

impl std::fmt::Display for InvalidVerifyRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}
impl std::error::Error for InvalidVerifyRequest {}

/// Validate and submit a verify request. Returns task_id on success.
/// On validation failure, logs to `audit:a2a:rejected` and returns error.
pub async fn submit_verify_request(
    relay: &A2ARelay,
    req: &VerifyRequest,
    requester_pair_id: &str,
) -> Result<String> {
    // Validate request
    if let Err(e) = relay.validate_verify_request(req, requester_pair_id) {
        // Log rejection to audit trail
        log_rejected_request(relay, req, &e.to_string()).await;
        return Err(InvalidVerifyRequest(e.to_string()).into());
    }

    // Idempotency: check if task already exists
    let task_id = relay.check_or_create_task(req, requester_pair_id).await?;

    // The executor (Forge) claims the pending task via `tasks/claim` and
    // runs it in its own workspace; Sentinel polls `tasks/get` for the
    // terminal state and mirrors the result to Redis.

    Ok(task_id)
}

/// Log a rejected request to the audit trail in Redis.
async fn log_rejected_request(relay: &A2ARelay, req: &VerifyRequest, reason: &str) {
    let entry = json!({
        "pair_id": &req.pair_id,
        "argv": &req.argv,
        "reason": reason,
        "timestamp": chrono::Utc::now().to_rfc3339(),
    });

    // Write last rejected request to audit:a2a:rejected for inspection.
    // Only the most recent rejection is kept — this is a debugging aid,
    // not an append-only log. A full audit trail requires Redis LIST ops
    // (LPUSH) which are not currently exposed by SharedStore.
    let rejected_key = a2a_protocol::audit_rejected_key();
    let entry_str = entry.to_string();
    relay
        .store()
        .set(rejected_key, serde_json::json!(entry_str))
        .await;

    warn!(
        pair_id = %req.pair_id,
        argv = ?req.argv,
        reason,
        "Verify request rejected and logged to audit trail"
    );
}
