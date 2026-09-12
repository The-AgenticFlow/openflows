// crates/agent-nexus/src/hooks/guard.rs
//! Slice C — stateful, phase-aware `pre_tool_use` write/bash guard.
//!
//! On top of the existing generic `apply_policy()` (rm -rf, force-push,
//! redis-cli, control-plane mutation, workspace escape), this adds per-role,
//! phase-aware decisions:
//!   - Forge in `planning` with no plan yet uploaded → the first writes must be
//!     the plan (PLAN.md / a harness `plan write`). Writes to source before a
//!     plan exists are denied.
//!   - Sentinel → all Write/Edit/Create/Patch are denied (readonly reviewer);
//!     read-only bash is allowed, destructive bash stays denied.
//!
//! The guard only reads Redis state; it never persists anything itself. The
//! confirmation of a successfully-placed first write arrives via `post_tool_use`
//! (Slice B) which observes the tool response.

use super::context::{read_ticket_state, resolve_chat};
use crate::hooks::types::HookDecision;
use pocketflow_core::SharedStore;
use serde_json::Value;

/// Is the tool a write-ish tool?
fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "write" | "edit" | "create" | "patch" | "Write" | "Edit" | "Create" | "Patch"
    )
}

/// Is the tool a harness coordination call?
fn is_harness(name: &str) -> bool {
    name.contains("openflows-harness") || name.contains("harness")
}

/// The path a write tool targets, from `tool_input`.
fn target_path(input: &Value) -> String {
    input
        .get("path")
        .or_else(|| input.get("file_path"))
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string()
}

/// True when the write is to / creates the plan artifact.
fn is_plan_write(role: &str, tool_name: &str, input: &Value) -> bool {
    if tool_name.contains("plan") || input.get("is_plan").is_some() {
        return true;
    }
    let path = target_path(input).to_lowercase();
    if path.ends_with("plan.md") || path.ends_with("plan") || path.contains("/plan") {
        return true;
    }
    // A harness `plan write` call uploads the plan.
    if is_harness(tool_name) && tool_name.contains("plan") {
        return true;
    }
    let _ = role;
    false
}

/// Stateful phase-aware guard. `base` is the generic policy decision already
/// computed for non-phase cases (typically `observe()`); if `base` denies, we
/// keep the denial and don't downgrade it.
pub async fn phase_guard(
    store: &SharedStore,
    chat_id: &str,
    role: &str,
    tool_name: &str,
    input: &Value,
    base: HookDecision,
) -> HookDecision {
    // Never downgrade an existing denial / rewrite from the generic policy.
    if base.deny || base.rewrite.is_some() {
        return base;
    }

    let lower = tool_name.to_lowercase();

    // SENTINEL: readonly reviewer → deny all writes.
    if role == "sentinel" && is_write_tool(&lower) {
        return HookDecision::deny(
            "openflows policy: SENTINEL is a readonly reviewer — writes are blocked",
        );
    }

    // FORGE planning gate: first write must be the plan.
    if role == "forge" && is_write_tool(&lower) {
        let hc = resolve_chat(store, chat_id).await;
        if let Some(ticket) = hc.ticket_id {
            let st = read_ticket_state(store, &ticket).await;
            if st.phase.as_deref() == Some("planning") && !st.plan_exists {
                if !is_plan_write(role, &lower, input) {
                    return HookDecision::deny(
                        "openflows policy: FORGE is in the planning phase and must write \
                         PLAN.md (or `openflows-harness plan write`) before touching source",
                    );
                }
            }
        }
    }

    // Non-mutating case (or any other role/tool) → fall through to base.
    base
}
