// crates/agent-nexus/src/hooks/guard.rs
//! Slice C — stateful, phase-aware `pre_tool_use` write/bash guard.
//!
//! On top of the generic `apply_policy()` (rm -rf, force-push, redis-cli, ...),
//! this adds per-role, phase-aware decisions for the **complete** Forge
//! lifecycle. The harness phases are authoritative:
//! `planning → building → testing → review_ready`, with `blocked` as the
//! failure escape hatch. There is no separate `pr_ready` phase: opening a PR is
//! the artifact produced *inside* `review_ready`.
//!
//! The guard asks one question of every tool call — "is the agent doing exactly
//! what its current phase says it should?":
//!   - `planning`     — first/next write must be the plan (PLAN.md / `plan write`).
//!   - `building`     — implementation; **the plan MUST already exist**. If a
//!     `building` ticket has no plan, the agent is out of sequence and is told to
//!     run `/plan` first (deny, relayed to the model via `post_tool_use`).
//!   - `testing`      — verify; plan must exist, source fixes permitted.
//!   - `review_ready` — terminal; source writes denied, rework re-enters earlier.
//!   - `blocked`      — only blocker report + read-only probes.
//!   - `sentinel`     — read-only reviewer; all writes denied.
//!
//! The guard only reads Redis state — it never persists. Its denial reasons are
//! also **guidelines** that `post_tool_use` collects and feeds back to the model.

use super::context::{read_ticket_state, resolve_chat};
use crate::hooks::types::HookDecision;
use pocketflow_core::SharedStore;
use serde_json::Value;

/// The phases of a Forge worker's lifecycle, in order.
const PHASES: &[&str] = &["planning", "building", "testing", "review_ready", "blocked"];

/// True iff `phase` is a recognised Forge lifecycle phase.
pub fn is_valid_phase(phase: &str) -> bool {
    PHASES.contains(&phase)
}

/// Is the tool a write-ish tool?
fn is_write_tool(name: &str) -> bool {
    matches!(
        name,
        "write" | "edit" | "create" | "patch" | "Write" | "Edit" | "Create" | "Patch"
    )
}

/// Is the tool the harness coordination CLI (status/plan/gate/pr/handoff)?
fn is_harness(name: &str) -> bool {
    name.to_lowercase().contains("harness")
}

/// Is the tool a shell (Bash/exec) call?
fn is_shell(name: &str) -> bool {
    matches!(
        name.to_lowercase().as_str(),
        "bash" | "sh" | "shell" | "execute" | "exec"
    )
}

/// Is the tool a read-only probe (bash read of status/dispatch/plan)?
fn is_probe_command(command: &str) -> bool {
    let cmd = command.to_lowercase();
    cmd.contains("status get")
        || cmd.contains("status set") // set is coordinated by the harness, not a source write
        || cmd.contains("gate status")
        || cmd.contains("dispatch read")
        || cmd.contains("plan read")
        || cmd.contains("plan write")
        || cmd.contains("review read")
        || cmd.contains("git status")
        || cmd.contains("git diff")
        || cmd == "ls"
}

fn command_text(input: &Value) -> String {
    if let Some(command) = input.get("command").and_then(|c| c.as_str()) {
        return command.to_string();
    }
    if let Some(argv) = input.get("argv").and_then(|v| v.as_array()) {
        return argv
            .iter()
            .filter_map(|v| v.as_str())
            .collect::<Vec<_>>()
            .join(" ");
    }
    input.as_str().unwrap_or_default().to_string()
}

/// Coordination commands that must remain available when durable state is
/// inconsistent, especially `phase=building` with a missing/undecodable plan.
fn is_plan_recovery_command(tool_name: &str, input: &Value) -> bool {
    let lower = tool_name.to_lowercase();
    let command = if is_shell(&lower) || is_harness(&lower) {
        command_text(input).to_lowercase()
    } else {
        String::new()
    };

    if command.is_empty() || !command.contains("openflows-harness") {
        return false;
    }

    command.contains("plan write")
        || command.contains("status set planning")
        || command.contains("status set blocked")
        || command.contains("status get")
        || command.contains("plan read")
        || command.contains("dispatch read")
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
fn is_plan_write(tool_name: &str, input: &Value) -> bool {
    if tool_name.to_lowercase().contains("plan") {
        return true;
    }
    if input.get("is_plan").is_some() {
        return true;
    }
    let path = target_path(input).to_lowercase();
    if path.ends_with("plan.md") || path.ends_with("plan") || path.contains("/plan") {
        return true;
    }
    // A harness `plan write` call uploads the plan.
    if is_harness(tool_name) {
        let cmd = input
            .get("command")
            .and_then(|c| c.as_str())
            .unwrap_or_default()
            .to_lowercase();
        if cmd.contains("plan write") {
            return true;
        }
    }
    false
}

/// Is a write targeting the blocker report (allowed in the `blocked` phase)?
fn is_blocker_write(tool_name: &str, input: &Value) -> bool {
    let path = target_path(input).to_lowercase();
    if path.ends_with("status.json") || path.ends_with("blocker") || path.contains("blocker") {
        return true;
    }
    let _ = tool_name;
    false
}

/// Phase-appropriate guidance string for the model, relayed via `post_tool_use`
/// and `user_prompt_submit` so the agent always knows what its lifecycle expects.
pub fn phase_guidance(phase: &str, plan_exists: bool, role: &str) -> String {
    if role.eq_ignore_ascii_case("sentinel") {
        return "You are SENTINEL, the read-only reviewer. Your job is to review \
                 plans and PRs and submit verdicts — you must not write or edit source. \
                 Read the plan/PR, run read-only checks, and use `openflows-harness \
                 review submit` / `gate approve`."
            .to_string();
    }

    match phase {
        "planning" if !plan_exists => {
            "FORGE planning phase: a plan does not exist yet. Run `/plan` now to \
             analyze the ticket and write PLAN.md, then `openflows-harness plan write \
             --file PLAN.md`, then `openflows-harness status set planning`, and HALT \
             for SENTINEL gate approval before touching any source file."
                .to_string()
        }
        "planning" => "FORGE planning phase: your plan is written and awaiting SENTINEL gate \
             approval. Do not write source yet. Run `openflows-harness gate status \
             --phase planning`; HALT for approval, then `status set building`."
            .to_string(),
        "building" if !plan_exists => {
            "FORGE is in the building phase but NO plan exists in the shared store. \
             This is out of sequence. Stop and run `/plan` first: write PLAN.md, \
             upload it with `openflows-harness plan write --file PLAN.md`, and obtain \
             SENTINEL gate approval before continuing to build."
                .to_string()
        }
        "building" => "FORGE building phase: implement per PLAN.md. Write source and tests, run \
             the test suite, and signal completion with `openflows-harness status set`."
            .to_string(),
        "testing" => "FORGE testing phase: you are verifying behavior. Run the test suite, fix \
             failing tests, and only then `openflows-harness status set review_ready` \
             and open the PR."
            .to_string(),
        "review_ready" => {
            "FORGE review_ready phase: a PR is open and SENTINEL is reviewing it. Do \
             not modify source while under review — rework must re-enter \
             `status set planning`/`building` first."
                .to_string()
        }
        "blocked" => "FORGE blocked phase: record an exact, answerable blocker (STATUS.json) \
             and wait for NEXUS/human intervention. Do not write source or build."
            .to_string(),
        _ => {
            format!(
                "Signal the harness phase with `openflows-harness status set <phase>` and \
                 work to the phase's contract. Valid phases: {}.",
                PHASES.join(", ")
            )
        }
    }
}

/// Stateful phase-aware guard. `base` is the generic policy decision already
/// computed for non-phase cases (typically `observe()`); if `base` denies, we
/// keep the denial and don't downgrade it.
///
/// Also returns a `model_context` guidance string (through the returned
/// decision) whenever a phase rule fires, so `post_tool_use` can relay the
/// guideline back to the model.
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
    if role.eq_ignore_ascii_case("sentinel") && is_write_tool(&lower) {
        return HookDecision::deny(
            "openflows policy: SENTINEL is a readonly reviewer — writes are blocked",
        )
        .with_model_context(phase_guidance("", false, role));
    }

    // Non-forge roles fall through to base.
    if !role.eq_ignore_ascii_case("forge") {
        return base;
    }

    // Resolve durable state.
    let hc = resolve_chat(store, chat_id).await;
    let Some(ticket) = hc.ticket_id else {
        return base;
    };
    let st = read_ticket_state(store, &ticket).await;
    let phase = st.phase.as_deref().unwrap_or("unset");
    let guidance = phase_guidance(phase, st.plan_exists, role);
    let mut decision = HookDecision::observe();

    // Only gate write-ish tools and harness/shell coordination. Non-write,
    // non-coordination tools (e.g. Read, MCP reads) fall through.
    if !is_write_tool(&lower) && !is_harness(&lower) && !is_shell(&lower) {
        return base;
    }

    // ── PLANNING ──────────────────────────────────────────────────────────
    if phase == "planning" {
        if is_write_tool(&lower) && !st.plan_exists && !is_plan_write(&lower, input) {
            // First write must be the plan.
            decision = HookDecision::deny(
                "openflows policy: FORGE is in the planning phase and must write \
                 PLAN.md (or `openflows-harness plan write`) before touching source",
            );
        }
        return decision.with_model_context(guidance);
    }

    // ── BUILDING ──────────────────────────────────────────────────────────
    if phase == "building" {
        if !st.plan_exists {
            if is_plan_write(&lower, input) || is_plan_recovery_command(&lower, input) {
                return decision.with_model_context(guidance);
            }
            // Out-of-sequence: trying to build before a plan was ever uploaded.
            decision = HookDecision::deny(
                "openflows policy: FORGE is in the building phase but no plan exists \
                 in the shared store. Write and upload a plan first: run `/plan`, then \
                 `openflows-harness plan write --file PLAN.md`.",
            );
        }
        // Otherwise building writes are allowed; attach guidance regardless.
        return decision.with_model_context(guidance);
    }

    // ── TESTING ───────────────────────────────────────────────────────────
    if phase == "testing" {
        if !st.plan_exists {
            if is_plan_write(&lower, input) || is_plan_recovery_command(&lower, input) {
                return decision.with_model_context(guidance);
            }
            decision = HookDecision::deny(
                "openflows policy: FORGE is in the testing phase but no plan exists. \
                 Leave testing: run `/plan` and get the plan approved before building.",
            );
        }
        return decision.with_model_context(guidance);
    }

    // ── REVIEW_READY ──────────────────────────────────────────────────────
    if phase == "review_ready" {
        if is_write_tool(&lower) {
            // Under review: source must not change; re-enter an earlier phase.
            decision = HookDecision::deny(
                "openflows policy: FORGE is in review_ready (PR under review). Do not \
                 modify source; for rework run `openflows-harness status set planning` \
                 (or `building`) first.",
            );
        }
        return decision.with_model_context(guidance);
    }

    // ── BLOCKED ───────────────────────────────────────────────────────────
    if phase == "blocked" {
        let is_coordination = is_harness(&lower)
            || (is_shell(&lower)
                && input
                    .get("command")
                    .and_then(|c| c.as_str())
                    .map(is_probe_command)
                    .unwrap_or(false));
        let is_blocker = is_write_tool(&lower) && is_blocker_write(&lower, input);
        if !is_coordination && !is_blocker {
            decision = HookDecision::deny(
                "openflows policy: FORGE is blocked. Only record a blocker (STATUS.json) \
                 and probe state; do not write source or build.",
            );
        }
        return decision.with_model_context(guidance);
    }

    // Unknown/unset phase → attach general guidance, do not deny.
    decision.with_model_context(guidance)
}
