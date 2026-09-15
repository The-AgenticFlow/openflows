// crates/agent-nexus/src/hooks/simulate.rs
//! Simulation / test emitter for the Coder agent lifecycle hook consumer.
//!
//! Coder's `chatd` signs hook dispatches with HS256 using the deployment
//! secret and POSTs them to `CODER_CHAT_HOOK_URL`. This module reproduces that
//! signing and delivery so you can exercise the OpenFlows consumer end-to-end
//! without a Coder server. It builds a JWT that mirrors Coder's contract:
//! `iss=coder`, `aud=CODER_CHAT_HOOK_URL`, `exp`, `jti=dispatch_id`,
//! `body_sha256`, plus a `sub` of the form `coder:chat:<chat_id>` and a `type`
//! claim matching the body event.

use super::jwt::sha256_hex;
use super::types::HookEvent;
use anyhow::{anyhow, Context, Result};
use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use serde_json::{json, Value};

/// Sign a lifecycle-hook JWT exactly like Coder's `chatd`.
pub fn sign_hook_jwt(
    secret: &str,
    hook_url: &str,
    dispatch_id: &str,
    chat_id: &str,
    event: &str,
    body_sha256: &str,
) -> Result<String> {
    let claims = json!({
        "iss": "coder",
        "aud": hook_url,
        "sub": format!("coder:chat:{chat_id}"),
        "exp": (Utc::now().timestamp() as usize) + 3600,
        "nbf": (Utc::now().timestamp() as usize) - 60,
        "jti": dispatch_id,
        "type": event,
        "body_sha256": body_sha256,
    });
    encode(
        &Header::new(Algorithm::HS256),
        &claims,
        &EncodingKey::from_secret(secret.as_bytes()),
    )
    .context("failed to sign hook JWT")
}

/// Build a sample event body for a given event name (mirrors Coder shapes).
pub fn build_event_body(event: &str, chat_id: &str, dispatch_id: &str) -> Result<Value> {
    let parsed: HookEvent = serde_json::from_value(json!(event))
        .map_err(|_| anyhow!("unknown hook event `{event}`"))?;

    let (data, schema) = match parsed {
        HookEvent::SessionStart => (json!({"source": "startup"}), json!({"schema_version": 1})),
        HookEvent::UserPromptSubmit => (
            json!({"prompt": "Implement the ticket and open a PR.", "parts": []}),
            json!({"schema_version": 1}),
        ),
        HookEvent::PreToolUse => (
            json!({
                "tool_use_id": "tool-1",
                "tool_name": "Bash",
                "tool_input": {"command": "openflows-harness status set building"}
            }),
            json!({"schema_version": 1}),
        ),
        HookEvent::PostToolUse => (
            json!({
                "tool_use_id": "tool-1",
                "tool_name": "Bash",
                "tool_response": {"exit_code": 0, "stdout": "ok"}
            }),
            json!({"schema_version": 1}),
        ),
        HookEvent::PreCompact | HookEvent::PostCompact | HookEvent::Stop => {
            (json!({}), json!({"schema_version": 1}))
        }
    };

    let meta = json!({
        "dispatch_id": dispatch_id,
        "chat_id": chat_id,
        "owner_id": "00000000-0000-0000-0000-000000000000",
        "workspace_id": "00000000-0000-0000-0000-000000000000",
        "turn_id": "00000000-0000-0000-0000-000000000000"
    });
    let meta = meta.merge(&schema);

    Ok(json!({
        "type": event,
        "meta": meta,
        "data": data
    }))
}

/// Internal helper to merge two JSON objects (used above for meta defaults).
trait Merge {
    fn merge(self, other: &Value) -> Value;
}

impl Merge for Value {
    fn merge(self, other: &Value) -> Value {
        let mut base = self;
        if let (Some(b), Some(o)) = (base.as_object_mut(), other.as_object()) {
            for (k, v) in o {
                b.insert(k.clone(), v.clone());
            }
        }
        base
    }
}

/// POST a signed lifecycle event to the configured hook URL.
///
/// Returns the HTTP status line of the consumer response for inspection.
pub async fn dispatch_simulated_event(
    hook_url: &str,
    secret: &str,
    event: &str,
    chat_id: &str,
    dispatch_id: &str,
) -> Result<(u16, String)> {
    let body = build_event_body(event, chat_id, dispatch_id)?;
    let body_bytes = serde_json::to_vec(&body).context("failed to serialize event body")?;
    let digest = sha256_hex(&body_bytes);

    let token = sign_hook_jwt(secret, hook_url, dispatch_id, chat_id, event, &digest)?;

    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .context("failed to build http client")?;

    let resp = client
        .post(hook_url)
        .header("Authorization", format!("Bearer {token}"))
        .header("Content-Type", "application/json")
        .body(body_bytes)
        .send()
        .await
        .with_context(|| format!("POST to {hook_url} failed"))?;

    let status = resp.status().as_u16();
    let text = resp.text().await.unwrap_or_default();
    Ok((status, text))
}
