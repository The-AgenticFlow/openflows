// crates/agent-nexus/src/hooks/stop.rs
//! Slice D — status-aware, authorized `stop` monitoring.
//!
//! Coder's `stop` is not mutable server-side (it cannot be denied), so client-side
//! enforcement stays in `stop_require_artifact.sh` (exits 2 to block a premature
//! stop). This server-side classification gives the Control plane and the human a
//! durable, reasoned record of *why* an agent wanted to stop, and — for
//! anticipated stops — publishes a kick so Nexus reconciles the paused agent
//! promptly (the "resume the paused forge" lifecycle).
//!
//! A stop is *anticipated* when the agent stopped at a designed handoff point:
//!   - forge HALTed at the planning gate (phase=planning, gate not approved),
//!   - forge reached review_ready (PR submitted),
//!   - blocked with a reason,
//!   - fuel exhausted.
//! Otherwise it is classified *premature* (no kick; the Controller's FlowRecovery
//! treats an unexpected stop as a stalled worker on its next poll).

use super::context::{read_ticket_state, resolve_chat};
use pocketflow_core::SharedStore;
use serde_json::{json, Value};

/// Classification of a `stop` event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StopClassification {
    PlannedHandoff,
    Premature,
    Unknown,
}

impl StopClassification {
    fn as_str(&self) -> &'static str {
        match self {
            StopClassification::PlannedHandoff => "planned_handoff",
            StopClassification::Premature => "premature",
            StopClassification::Unknown => "unknown",
        }
    }
}

/// Classify a stop based on read-only durable state.
pub async fn classify_stop(store: &SharedStore, chat_id: &str) -> (StopClassification, Value) {
    let hc = resolve_chat(store, chat_id).await;
    let Some(ticket) = hc.ticket_id else {
        return (StopClassification::Unknown, json!({}));
    };
    let st = read_ticket_state(store, &ticket).await;
    let phase = st.phase.clone().unwrap_or_else(|| "unset".to_string());

    let classification = match phase.as_str() {
        "planning" if !st.gate_approved => StopClassification::PlannedHandoff,
        "review_ready" if st.pr_recorded => StopClassification::PlannedHandoff,
        "blocked" => StopClassification::PlannedHandoff,
        "fuel_exhausted" => StopClassification::PlannedHandoff,
        _ => StopClassification::Premature,
    };

    let detail = json!({
        "ticket_id": ticket,
        "role": hc.role.unwrap_or_default(),
        "phase": phase,
        "plan_exists": st.plan_exists,
        "gate_approved": st.gate_approved,
        "pr_recorded": st.pr_recorded,
        "handoff_exists": st.handoff_exists,
        "classification": classification.as_str(),
    });

    (classification, detail)
}

/// Build the durable audit record for a stop. Persists under
/// `ticket:{T}:hooks:stop`. Returns the HookKick to publish if the stop is a
/// planned handoff, else `None`.
pub fn stop_audit_record(
    classification: &StopClassification,
    detail: &Value,
) -> serde_json::Value {
    json!({
        "classification": classification.as_str(),
        "detail": detail,
        "ts": chrono::Utc::now().timestamp(),
    })
}
