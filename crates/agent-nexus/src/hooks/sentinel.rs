// crates/agent-nexus/src/hooks/sentinel.rs
//! Sentinel lifecycle — phase-aware `pre_tool_use` guard for the reviewer role.
//!
//! Sentinel has no `status set` phases of its own: its "job" is *derived* from
//! the shared ticket's durable state (what FORGE is doing). Two review jobs:
//!   - `plan_gate`  — FORGE is `planning` and the gate is not approved yet →
//!                    review `PLAN.md`, then `gate approve`.
//!   - `pr_review`  — FORGE is `review_ready` with a PR → review the diff, write
//!                    the evaluation report, then `review submit`.
//!   - `idle`       — nothing pending; no review action should be taken.
//!
//! Rules mirror FORGE's phase guard but for the reviewing direction:
//!   - Sentinel is read-only for source; only review artifacts (*-eval.md,
//!     final-review.md) may be written.
//!   - In `plan_gate` a `gate approve` requires a plan to have been uploaded.
//!   - In `pr_review` a `review submit` requires the evaluation report to have
//!     been written first (tracked on `post_tool_use`).

use super::context::{review_report_exists, TicketState};
use crate::hooks::types::HookDecision;
use pocketflow_core::SharedStore;
use serde_json::Value;

/// Sentinel's derived review job for the current shared ticket state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentinelJob {
    PlanGateReview,
    PrReview,
    Idle,
}

/// Derive what review job Sentinel should be doing from the ticket state.
pub fn sentinel_job(st: &TicketState) -> SentinelJob {
    match st.phase.as_deref() {
        Some("planning") if !st.gate_approved => SentinelJob::PlanGateReview,
        Some("review_ready") => SentinelJob::PrReview,
        _ => SentinelJob::Idle,
    }
}

/// Files Sentinel is allowed to write despite the read-only policy: these are
/// its evaluation/verdict artifacts, never source.
fn is_review_artifact(path: &str) -> bool {
    let p = path.to_lowercase();
    p.ends_with("-eval.md")
        || p.ends_with("eval.md")
        || p.ends_with("final-review.md")
        || p.ends_with("review.md")
        || p.contains("review-report")
}

/// Sentinel-level guidance injected so the model knows what job it is on.
pub fn sentinel_guidance(st: &TicketState) -> String {
    match sentinel_job(st) {
        SentinelJob::PlanGateReview => {
            "SENTINEL plan-gate review: FORGE is in `planning` and awaits approval. \
             Read PLAN.md, evaluate it against the ticket, then run \
             `openflows-harness gate approve --phase planning`."
                .to_string()
        }
        SentinelJob::PrReview => {
            "SENTINEL PR review: FORGE is `review_ready` with a PR to review. Read the \
             ticket + diff, write your evaluation report (*-eval.md / final-review.md), \
             then `openflows-harness review submit --verdict approve|reject`."
                .to_string()
        }
        SentinelJob::Idle => {
            "SENTINEL: no review is pending right now. Do not submit a gate approval or \
             review verdict until FORGE signals `planning` or `review_ready`."
                .to_string()
        }
    }
}

/// Sentinel-only phase-aware decision for a `pre_tool_use` dispatch. Sentinel is
/// read-only (denies source writes) and must follow its review job.
pub async fn sentinel_phase_guard(
    store: &SharedStore,
    ticket_id: &str,
    st: &TicketState,
    tool_name: &str,
    input: &Value,
    base: HookDecision,
) -> HookDecision {
    let lower = tool_name.to_lowercase();
    let guidance = sentinel_guidance(st);

    // Source writes are always denied for Sentinel; review artifacts are allowed.
    let is_write = matches!(lower.as_str(), "write" | "edit" | "create" | "patch");
    if is_write {
        let path = input
            .get("path")
            .or_else(|| input.get("file_path"))
            .and_then(|v| v.as_str())
            .unwrap_or("");
        if is_review_artifact(path) {
            return base.with_model_context(guidance);
        }
        return HookDecision::deny(
            "openflows policy: SENTINEL is a readonly reviewer — source writes are blocked \
             (only *-eval.md / final-review.md review reports may be written)",
        )
        .with_model_context(guidance);
    }

    // Only coordinate on review/harness tools.
    if !lower.contains("harness") && !matches!(lower.as_str(), "bash" | "sh" | "shell" | "exec") {
        return base;
    }
    let command = input
        .get("command")
        .and_then(|c| c.as_str())
        .unwrap_or_default()
        .to_lowercase();

    // Gate approval requires an uploaded plan.
    if command.contains("gate approve")
        && sentinel_job(st) == SentinelJob::PlanGateReview
        && !st.plan_exists
    {
        return HookDecision::deny(
            "openflows policy: cannot approve the planning gate — no plan exists yet. \
             Ask FORGE to run `/plan` and upload PLAN.md first.",
        )
        .with_model_context(guidance);
    }

    // A review verdict requires the evaluation report to be written first.
    if command.contains("review submit") && sentinel_job(st) == SentinelJob::PrReview {
        if !review_report_exists(store, ticket_id).await {
            return HookDecision::deny(
                "openflows policy: cannot submit a review yet — write your evaluation \
                 report (*-eval.md / final-review.md) first, then `review submit`.",
            )
            .with_model_context(guidance);
        }
    }

    // A verdict submitted outside the PR-review job is out of order.
    if command.contains("review submit")
        && sentinel_job(st) != SentinelJob::PrReview
        && !(command.contains("gate approve") || command.contains("gate status"))
    {
        return HookDecision::deny(
            "openflows policy: no PR review pending — do not submit a review verdict now.",
        )
        .with_model_context(guidance);
    }

    base.with_model_context(guidance)
}
