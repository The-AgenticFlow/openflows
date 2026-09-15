// crates/agent-nexus/src/hooks/slice_tests.rs
//! Tests for the experimental hook-driven behaviour slices A–D.

use super::bootstrap::{build_bootstrap_context, trim_to_limit, HookBootstrapContext};
use super::context::{drain_guidance, read_ticket_state, record_guidance, resolve_chat};
use super::guard::{is_valid_phase, phase_guard};
use super::kick::maybe_publish_kick;
use super::sentinel::{sentinel_job, sentinel_phase_guard, SentinelJob};
use super::stop::{classify_stop, StopClassification};
use super::types::HookDecision;
use pocketflow_core::{build_kick_bus, HookKick, SharedStore};
use serde_json::{json, Value};
use std::path::PathBuf;

/// Seed a `ticket:{T}:chat:{role}` = chat_id mapping (+ a status) into the store.
async fn seed_chat(store: &SharedStore, ticket: &str, role: &str, chat_id: &str) {
    store
        .set(&format!("ticket:{ticket}:chat:{role}"), json!(chat_id))
        .await;
}

async fn seed_status(store: &SharedStore, ticket: &str, phase: &str) {
    store
        .set(
            &format!("ticket:{ticket}:status"),
            json!({ "phase": phase }),
        )
        .await;
}

// ── context ─────────────────────────────────────────────────────────────

#[tokio::test]
async fn resolves_chat_to_role_and_ticket() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-1", "forge", "chat-1").await;
    let hc = resolve_chat(&store, "chat-1").await;
    assert_eq!(hc.role.as_deref(), Some("forge"));
    assert_eq!(hc.ticket_id.as_deref(), Some("T-1"));

    let missing = resolve_chat(&store, "chat-unknown").await;
    assert!(missing.role.is_none());
}

// ── Slice A: bootstrap ──────────────────────────────────────────────────

#[tokio::test]
async fn bootstrap_builds_context_with_resume_state() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-2", "sentinel", "chat-2").await;
    seed_status(&store, "T-2", "planning").await;

    let tmp = std::env::temp_dir();
    let ctx = HookBootstrapContext {
        persona_by_role: {
            let mut m = std::collections::HashMap::new();
            // A real temp persona file so we can read a trim.
            let p = tmp.join("sentinel-trim-persona.md");
            std::fs::write(&p, "# SENTINEL persona\nrigorous reviewer").unwrap();
            m.insert("sentinel".to_string(), p);
            m
        },
        skills_dir: Some(PathBuf::from("/nonexistent-skills")),
        commands_dir: Some(PathBuf::from("/nonexistent-commands")),
    };

    let mut dispatch = json!({});
    dispatch["title"] = json!("Implement X");

    let ctx_str = build_bootstrap_context(&store, "chat-2", &dispatch, &ctx)
        .await
        .expect("bootstrap should resolve for a known chat");
    assert!(ctx_str.contains("SENTINEL persona"));
    assert!(ctx_str.contains("T-2"));
    assert!(ctx_str.contains("planning"));
}

#[test]
fn trim_to_limit_caps_at_16k() {
    let big = "x".repeat(20_000);
    let out = trim_to_limit(&big);
    assert!(out.len() <= super::bootstrap::MODEL_CONTEXT_BYTE_LIMIT);
    assert!(out.ends_with("...[trimmed to model_context limit]"));
}

// ── Slice C: phase guard ────────────────────────────────────────────────

#[tokio::test]
async fn phase_guard_denies_source_write_before_plan() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-3", "forge", "chat-3").await;
    seed_status(&store, "T-3", "planning").await;

    let input = json!({ "path": "src/main.rs" });
    // Simulate a Write; base is observe() (no generic deny).
    let base = HookDecision::observe();
    let d = phase_guard(&store, "chat-3", "forge", "write", &input, base).await;
    assert!(
        d.deny,
        "forge in planning must not write source before a plan exists"
    );
}

#[tokio::test]
async fn phase_guard_allows_plan_write_before_plan() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-4", "forge", "chat-4").await;
    seed_status(&store, "T-4", "planning").await;

    let input = json!({ "path": "PLAN.md" });
    let base = HookDecision::observe();
    let d = phase_guard(&store, "chat-4", "forge", "write", &input, base).await;
    assert!(!d.deny, "writing PLAN.md is allowed in planning");
}

#[tokio::test]
async fn phase_guard_allows_source_write_once_plan_exists() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-5", "forge", "chat-5").await;
    seed_status(&store, "T-5", "planning").await;
    store
        .set("pair:T-5:plan", json!({ "content": "# plan" }))
        .await;

    let input = json!({ "path": "src/main.rs" });
    let base = HookDecision::observe();
    let d = phase_guard(&store, "chat-5", "forge", "write", &input, base).await;
    assert!(!d.deny, "source writes allowed once a plan exists");
}

#[tokio::test]
async fn phase_guard_denies_all_sentinel_writes() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-6", "sentinel", "chat-6").await;

    let input = json!({ "path": "src/main.rs" });
    let base = HookDecision::observe();
    let d = phase_guard(&store, "chat-6", "sentinel", "edit", &input, base).await;
    assert!(d.deny, "sentinel is readonly");
}

// ── Slice B: kick publishing ────────────────────────────────────────────

#[tokio::test]
async fn post_tool_use_recognition_kicks_on_verdict() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-7", "sentinel", "chat-7").await;
    let (publisher, mut receiver) = build_kick_bus(None, "default").await.unwrap();

    let data = json!({
        "tool_name": "Bash",
        "tool_input": { "command": "openflows-harness review submit --verdict approve" },
        "tool_response": { "exit_code": 0 }
    });

    maybe_publish_kick(
        &publisher,
        &store,
        "chat-7",
        "dis-7",
        super::types::HookEvent::PostToolUse,
        &data,
    )
    .await;

    let kick: HookKick = receiver.recv().await.expect("a kick should be published");
    assert_eq!(kick.hint, "verdict_written");
    assert_eq!(kick.role, "sentinel");
    assert_eq!(kick.ticket_id, "T-7");
}

#[tokio::test]
async fn post_tool_use_non_mutating_does_not_kick() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-8", "forge", "chat-8").await;
    let (publisher, mut receiver) = build_kick_bus(None, "default").await.unwrap();

    let data = json!({
        "tool_name": "Bash",
        "tool_input": { "command": "cargo build" },
        "tool_response": { "exit_code": 0 }
    });

    maybe_publish_kick(
        &publisher,
        &store,
        "chat-8",
        "dis-8",
        super::types::HookEvent::PostToolUse,
        &data,
    )
    .await;

    // Should not publish for a non-mutating command.
    let timeout = tokio::time::timeout(std::time::Duration::from_millis(50), receiver.recv()).await;
    assert!(timeout.is_err(), "no kick expected for cargo build");
}

// ── Slice D: stop classification ────────────────────────────────────────

#[tokio::test]
async fn stop_review_ready_with_pr_is_planned() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-9", "forge", "chat-9").await;
    seed_status(&store, "T-9", "review_ready").await;
    store.set("ticket:T-9:pr", json!({ "pr_number": 5 })).await;

    let (classification, detail) = classify_stop(&store, "chat-9").await;
    assert_eq!(classification, StopClassification::PlannedHandoff);
    assert_eq!(detail["pr_recorded"], true);
}

#[tokio::test]
async fn stop_mid_build_without_pr_is_premature() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-10", "forge", "chat-10").await;
    seed_status(&store, "T-10", "building").await;

    let (classification, _detail) = classify_stop(&store, "chat-10").await;
    assert_eq!(classification, StopClassification::Premature);
}

// ── UTF-8-safe truncation (SessionStart crash fix) ───────────────────────

#[test]
fn truncate_utf8_does_not_panic_on_multibyte_boundary() {
    // Em-dash + arrow are multi-byte; a naive &s[..limit] would panic mid-char.
    let s = format!("{}—→🔧 END", "x".repeat(16_500));
    let out = trim_to_limit(&s);
    assert!(!out.contains("END"), "tail must be trimmed at cap");
    assert!(out.len() <= super::bootstrap::MODEL_CONTEXT_BYTE_LIMIT);
    assert!(
        out.is_char_boundary(out.len()),
        "truncation must be UTF-8-safe"
    );
}

#[tokio::test]
async fn bootstrap_with_commands_content_survives_limit() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-11", "forge", "chat-11").await;

    let tmp = std::env::temp_dir().join("hook-cmds-forge");
    let _ = std::fs::remove_dir_all(&tmp);
    std::fs::create_dir_all(&tmp).unwrap();
    std::fs::write(
        tmp.join("plan.md"),
        "# /plan\nRun /plan first — write PLAN.md — 🔧\n",
    )
    .unwrap();
    std::fs::write(
        tmp.join("status.md"),
        "# /status\nopenflows-harness status set <phase>\n",
    )
    .unwrap();

    let ctx = HookBootstrapContext {
        persona_by_role: std::collections::HashMap::new(),
        skills_dir: None,
        commands_dir: Some(tmp),
    };
    let dispatch = json!({ "title": "Implement X" });
    let out = build_bootstrap_context(&store, "chat-11", &dispatch, &ctx)
        .await
        .expect("should resolve");
    assert!(
        out.contains("/plan"),
        "plan command content should be embedded"
    );
    assert!(out.contains("T-11"));
    assert!(out.is_char_boundary(out.len()));
}

// ── Forge building-without-plan guard ────────────────────────────────────

#[tokio::test]
async fn phase_guard_denies_building_without_plan() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-12", "forge", "chat-12").await;
    seed_status(&store, "T-12", "building").await;

    let input = json!({ "path": "src/main.rs" });
    let d = phase_guard(
        &store,
        "chat-12",
        "forge",
        "write",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(d.deny, "building without a plan must be denied");
    assert!(
        d.model_context
            .as_deref()
            .map(|c| c.contains("/plan"))
            .unwrap_or(false),
        "guidance should tell forge to run /plan"
    );
}

#[tokio::test]
async fn phase_guard_allows_missing_plan_recovery_commands() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-18", "forge", "chat-18").await;
    seed_status(&store, "T-18", "building").await;

    for command in [
        "openflows-harness plan write --file /home/coder/workspace/PLAN.md",
        "openflows-harness status set planning",
        "openflows-harness status set blocked",
    ] {
        let input = json!({ "command": command });
        let d = phase_guard(
            &store,
            "chat-18",
            "forge",
            "bash",
            &input,
            HookDecision::observe(),
        )
        .await;
        assert!(!d.deny, "{command} should stay available for recovery");
    }
}

#[tokio::test]
async fn ticket_state_counts_plan_key_as_existing() {
    let store = SharedStore::new_in_memory();
    store
        .set("pair:T-19:plan", json!("# PLAN\n\nRaw markdown"))
        .await;

    let st = read_ticket_state(&store, "T-19").await;
    assert!(st.plan_exists, "plan key presence should count as present");
}

#[test]
fn recognises_valid_phases() {
    assert!(is_valid_phase("planning"));
    assert!(is_valid_phase("building"));
    assert!(is_valid_phase("testing"));
    assert!(is_valid_phase("review_ready"));
    assert!(is_valid_phase("blocked"));
    assert!(!is_valid_phase("pr_ready"));
}

// ── Sentinel lifecycle ───────────────────────────────────────────────────

async fn seed_ticket_state(
    store: &SharedStore,
    ticket: &str,
    phase: &str,
    plan: bool,
    gate: bool,
    pr: bool,
) {
    seed_status(store, ticket, phase).await;
    if plan {
        store
            .set(
                &format!("pair:{ticket}:plan"),
                json!({ "content": "# plan" }),
            )
            .await;
    }
    if gate {
        store
            .set(
                &format!("ticket:{ticket}:gate:planning"),
                json!({ "approved_by": "sentinel" }),
            )
            .await;
    }
    if pr {
        store
            .set(&format!("ticket:{ticket}:pr"), json!({ "pr_number": 7 }))
            .await;
    }
}

#[tokio::test]
async fn sentinel_job_derives_plan_gate_vs_pr_review() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-13", "sentinel", "chat-13").await;

    seed_ticket_state(&store, "T-13", "planning", true, false, false).await;
    assert_eq!(
        sentinel_job(&read_ticket_state(&store, "T-13").await),
        SentinelJob::PlanGateReview
    );

    seed_ticket_state(&store, "T-13", "review_ready", true, true, true).await;
    assert_eq!(
        sentinel_job(&read_ticket_state(&store, "T-13").await),
        SentinelJob::PrReview
    );

    seed_ticket_state(&store, "T-13", "building", true, true, false).await;
    assert_eq!(
        sentinel_job(&read_ticket_state(&store, "T-13").await),
        SentinelJob::Idle
    );
}

#[tokio::test]
async fn sentinel_denies_gate_approve_when_no_plan() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-14", "sentinel", "chat-14").await;
    seed_ticket_state(&store, "T-14", "planning", false, false, false).await;
    let st = read_ticket_state(&store, "T-14").await;

    let input = json!({ "command": "openflows-harness gate approve --phase planning" });
    let d =
        sentinel_phase_guard(&store, "T-14", &st, "Bash", &input, HookDecision::observe()).await;
    assert!(d.deny, "cannot approve gate before a plan exists");
}

#[tokio::test]
async fn sentinel_allows_eval_report_write_but_denies_source() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-15", "sentinel", "chat-15").await;
    seed_ticket_state(&store, "T-15", "review_ready", true, true, true).await;
    let st = read_ticket_state(&store, "T-15").await;

    let report = json!({ "path": "final-review.md" });
    let d1 = sentinel_phase_guard(
        &store,
        "T-15",
        &st,
        "Write",
        &report,
        HookDecision::observe(),
    )
    .await;
    assert!(!d1.deny, "sentinel may write its eval report");

    let source = json!({ "path": "src/main.rs" });
    let d2 = sentinel_phase_guard(
        &store,
        "T-15",
        &st,
        "Write",
        &source,
        HookDecision::observe(),
    )
    .await;
    assert!(d2.deny, "sentinel may not write source");
}

#[tokio::test]
async fn sentinel_review_submit_requires_report_first() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-16", "sentinel", "chat-16").await;
    seed_ticket_state(&store, "T-16", "review_ready", true, true, true).await;

    let st = read_ticket_state(&store, "T-16").await;
    let submit = json!({ "command": "openflows-harness review submit --verdict approve" });

    // No report written yet → denied.
    let d1 = sentinel_phase_guard(
        &store,
        "T-16",
        &st,
        "Bash",
        &submit,
        HookDecision::observe(),
    )
    .await;
    assert!(d1.deny, "review submit needs a report first");

    // After a report marker is recorded → allowed.
    super::context::record_review_report_marker(&store, "T-16").await;
    let st2 = read_ticket_state(&store, "T-16").await;
    let d2 = sentinel_phase_guard(
        &store,
        "T-16",
        &st2,
        "Bash",
        &submit,
        HookDecision::observe(),
    )
    .await;
    assert!(!d2.deny, "review submit allowed after report is written");
}

// ── post_tool_use guidance relay ─────────────────────────────────────────

#[tokio::test]
async fn guidance_recorded_then_drained() {
    let store = SharedStore::new_in_memory();
    record_guidance(&store, "T-17", json!({ "reason": "write PLAN.md first" })).await;
    let drained: Vec<Value> = drain_guidance(&store, "T-17").await;
    assert_eq!(drained.len(), 1);
    assert_eq!(drained[0]["reason"], "write PLAN.md first");
    // Second drain is empty.
    let again = drain_guidance(&store, "T-17").await;
    assert!(again.is_empty());
}
