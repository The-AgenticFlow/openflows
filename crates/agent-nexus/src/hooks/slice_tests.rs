// crates/agent-nexus/src/hooks/slice_tests.rs
//! Tests for the experimental hook-driven behaviour slices A–D.

use super::bootstrap::{build_bootstrap_context, HookBootstrapContext, trim_to_limit};
use super::context::resolve_chat;
use super::guard::phase_guard;
use super::kick::maybe_publish_kick;
use super::stop::{classify_stop, StopClassification};
use super::types::HookDecision;
use pocketflow_core::{build_kick_bus, HookKick, SharedStore};
use serde_json::json;
use std::path::PathBuf;

/// Seed a `ticket:{T}:chat:{role}` = chat_id mapping (+ a status) into the store.
async fn seed_chat(store: &SharedStore, ticket: &str, role: &str, chat_id: &str) {
    store
        .set(&format!("ticket:{ticket}:chat:{role}"), json!(chat_id))
        .await;
}

async fn seed_status(store: &SharedStore, ticket: &str, phase: &str) {
    store
        .set(&format!("ticket:{ticket}:status"), json!({ "phase": phase }))
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
    assert!(out.len() <= super::bootstrap::MODEL_CONTEXT_BYTE_LIMIT + 40);
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
    assert!(d.deny, "forge in planning must not write source before a plan exists");
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
    store.set("pair:T-5:plan", json!({ "content": "# plan" })).await;

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

    maybe_publish_kick(&publisher, &store, "chat-7", "dis-7", super::types::HookEvent::PostToolUse, &data).await;

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

    maybe_publish_kick(&publisher, &store, "chat-8", "dis-8", super::types::HookEvent::PostToolUse, &data).await;

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
