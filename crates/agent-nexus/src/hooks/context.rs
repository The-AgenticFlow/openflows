// crates/agent-nexus/src/hooks/context.rs
//! Hook event context resolution: from a Coder `chat_id` back to the OpenFlows
//! role + ticket + per-ticket durable state, read-only from the SharedStore.
//!
//! Coder's lifecycle events carry `chat_id` but not role/ticket (D1/D2 in
//! `docs/experiments/coder-lifecycle-hooks-feedback.md`). The consumer resolves
//! them by scanning the `ticket:{T}:chat:{role}` keys the Controller writes in
//! `NexusNode::create_chat_for_assignment`. Resolution is read-only and
//! best-effort: a failed lookup yields `None`s and callers fail open.

use pocketflow_core::SharedStore;
use serde_json::Value;

/// Resolved role + ticket for a hook dispatch.
#[derive(Debug, Clone, Default)]
pub struct HookContext {
    pub role: Option<String>,
    pub ticket_id: Option<String>,
    pub workspace_id: Option<String>,
}

/// Read the relevant per-ticket durable state for the hook slices.
#[derive(Debug, Clone, Default)]
pub struct TicketState {
    /// `ticket:{T}:status` → `.phase` (planning / building / testing /
    /// review_ready / blocked).
    pub phase: Option<String>,
    /// Whether `pair:{T}:plan` exists (a plan has been uploaded).
    pub plan_exists: bool,
    /// Whether `ticket:{T}:gate:planning` is set (SENTINEL approved the gate).
    pub gate_approved: bool,
    /// `ticket:{T}:pr` presence (a PR has been recorded).
    pub pr_recorded: bool,
    /// `ticket:{T}:handoff` presence.
    pub handoff_exists: bool,
    /// `ticket:{T}:basic_status` (fuel / TS-strings) flattened map, if any.
    pub flags: Value,
}

/// Key under which per-ticket pre-tool guidance/denials accumulate, drained by
/// `post_tool_use` and fed back to the model as a safety net.
pub const GUIDANCE_KEY: &str = "hooks:guidance";
/// Max guidance records kept per ticket before the tail is trimmed.
const GUIDANCE_TAIL: usize = 8;

/// Append a guidance/error record produced by a `pre_tool_use` policy check to
/// the ticket's guidance tail, drained by `post_tool_use` to relay to the model.
pub async fn record_guidance(store: &SharedStore, ticket_id: &str, guidance: Value) {
    let key = format!("ticket:{ticket_id}:{GUIDANCE_KEY}");
    let mut tail: Vec<Value> = store.get_typed(&key).await.unwrap_or_default();
    tail.push(guidance);
    if tail.len() > GUIDANCE_TAIL {
        let drop = tail.len() - GUIDANCE_TAIL;
        tail.drain(0..drop);
    }
    store.set_typed(&key, &tail).await.ok();
}

/// Read and clear the ticket's pending guidance tail. Returns the drained
/// records so a caller (`post_tool_use`) can relay them to the model, and an
/// empty vec when there is nothing pending. Clearing is best-effort.
pub async fn drain_guidance(store: &SharedStore, ticket_id: &str) -> Vec<Value> {
    let key = format!("ticket:{ticket_id}:{GUIDANCE_KEY}");
    let tail: Vec<Value> = store.get_typed(&key).await.unwrap_or_default();
    if !tail.is_empty() {
        store.del(&key).await;
    }
    tail
}

/// Key tracking that SENTINEL wrote its evaluation report for the active PR
/// review; cleared when a fresh review cycle begins.
const REVIEW_REPORT_KEY: &str = "hooks:review_report";

/// True when SENTINEL has written an evaluation report (observed via
/// `post_tool_use`) that must precede a `review submit`.
pub async fn review_report_exists(store: &SharedStore, ticket_id: &str) -> bool {
    store
        .get(&format!("ticket:{ticket_id}:{REVIEW_REPORT_KEY}"))
        .await
        .is_some()
}

/// Mark that a review report was written. Called by `post_tool_use` after a
/// SENTINEL write to a `*-eval.md` / `final-review.md` path.
pub async fn record_review_report_marker(store: &SharedStore, ticket_id: &str) {
    store
        .set(
            &format!("ticket:{ticket_id}:{REVIEW_REPORT_KEY}"),
            Value::Bool(true),
        )
        .await;
}

/// Resolve role + ticket from a chat id by scanning this tenant's chat keys.
pub async fn resolve_chat(store: &SharedStore, chat_id: &str) -> HookContext {
    if chat_id.is_empty() {
        return HookContext::default();
    }
    // `keys` applies the store's tenant namespace and returns full keys like
    // `ns:{tenant}:ticket:{T}:chat:{role}`. This avoids scanning unrelated
    // tenants on high-frequency hook dispatches.
    let keys = store.keys("ticket:*").await;
    for key in keys {
        if !key.contains(":chat:") {
            continue;
        }
        let un_ns = strip_namespace(&key);
        let stored: Option<String> = store.get_typed(&un_ns).await;
        if stored.as_deref() == Some(chat_id) {
            return HookContext {
                role: role_from_key(&un_ns),
                ticket_id: ticket_from_key(&un_ns),
                workspace_id: None,
            };
        }
    }
    HookContext::default()
}

/// Strip the leading `ns:{tenant}:` prefix from a raw key. The un-namespaced key
/// starts at the `ticket` segment (the store builds `ns:{tenant}:{key}`), so we
/// find that marker rather than knowing the tenant.
fn strip_namespace(raw: &str) -> String {
    let segs = raw.split(':').collect::<Vec<_>>();
    if let Some(idx) = segs.iter().position(|&s| s == "ticket") {
        segs[idx..].join(":")
    } else {
        raw.to_string()
    }
}

/// Pull the ticket id out of a `ticket:{T}:chat:{role}` key.
fn ticket_from_key(un_ns: &str) -> Option<String> {
    let seg = un_ns.split(':').collect::<Vec<_>>();
    let idx = seg.iter().position(|&s| s == "ticket")?;
    seg.get(idx + 1).map(|s| s.to_string())
}

/// Pull the role out of a `ticket:{T}:chat:{role}` key.
fn role_from_key(un_ns: &str) -> Option<String> {
    let seg = un_ns.split(':').collect::<Vec<_>>();
    let idx = seg.iter().position(|&s| s == "chat")?;
    seg.get(idx + 1).map(|s| s.to_string())
}

/// Read the durable per-ticket state used by the hook slices.
pub async fn read_ticket_state(store: &SharedStore, ticket_id: &str) -> TicketState {
    let lifecycle = store.lifecycle(ticket_id).await.ok();
    let phase = lifecycle.as_ref().map(|s| s.phase.as_str().to_owned());
    let plan_key = format!("pair:{ticket_id}:plan");
    let plan_exists = lifecycle.as_ref().is_some_and(|s| !s.plan.is_empty())
        || !store.keys(&plan_key).await.is_empty();
    let gate_approved = lifecycle
        .as_ref()
        .is_some_and(|s| s.plan_decision.as_ref().is_some_and(|d| d.approved));
    let pr_key = format!("ticket:{ticket_id}:pr");
    let pr_recorded = store.get(&pr_key).await.is_some();

    let handoff_key = format!("ticket:{ticket_id}:handoff");
    let handoff_exists = store.get(&handoff_key).await.is_some();

    let flags = store
        .get(&format!("ticket:{ticket_id}:basic_status"))
        .await
        .unwrap_or(Value::Null);

    TicketState {
        phase,
        plan_exists,
        gate_approved,
        pr_recorded,
        handoff_exists,
        flags,
    }
}
