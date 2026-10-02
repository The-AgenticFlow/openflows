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
            serde_json::to_value(config::lifecycle::Lifecycle {
                phase: config::lifecycle::Phase::parse(phase).unwrap(),
                ..Default::default()
            })
            .unwrap(),
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
        skills_by_role: Default::default(),
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

    let input = json!({ "path": "/home/coder/.coder/plans/PLAN-chat-4.md" });
    let base = HookDecision::observe();
    let d = phase_guard(&store, "chat-4", "forge", "write", &input, base).await;
    assert!(!d.deny, "writing the standard plan is allowed in planning");
}

#[tokio::test]
async fn phase_guard_rejects_plan_named_source_file_before_plan() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-20", "forge", "chat-20").await;
    seed_status(&store, "T-20", "planning").await;

    let input = json!({ "path": "src/plan_helpers.rs", "is_plan": false });
    let d = phase_guard(
        &store,
        "chat-20",
        "forge",
        "write",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(d.deny, "source paths containing 'plan' are not PLAN.md");
}

#[tokio::test]
async fn phase_guard_denies_shell_source_write_before_plan() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-21", "forge", "chat-21").await;
    seed_status(&store, "T-21", "planning").await;

    let input = json!({ "command": "printf 'oops' > src/lib.rs" });
    let d = phase_guard(
        &store,
        "chat-21",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(d.deny, "shell redirection source writes are write attempts");
}

#[tokio::test]
async fn phase_guard_denies_shell_copy_from_plan_to_source() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-22", "forge", "chat-22").await;
    seed_status(&store, "T-22", "planning").await;

    let input = json!({ "command": "cp PLAN.md src/lib.rs" });
    let d = phase_guard(
        &store,
        "chat-22",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        d.deny,
        "artifact source operands must not hide source write targets"
    );
}

#[tokio::test]
async fn phase_guard_allows_shell_write_to_plan_artifact() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-23", "forge", "chat-23").await;
    seed_status(&store, "T-23", "planning").await;

    let input = json!({ "command": "printf '# plan' > /home/coder/.coder/plans/PLAN-chat-23.md" });
    let d = phase_guard(
        &store,
        "chat-23",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(!d.deny, "shell writes to the standard plan stay allowed");
}

#[tokio::test]
async fn phase_guard_denies_source_write_until_plan_approved() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-5", "forge", "chat-5").await;
    seed_status(&store, "T-5", "planning").await;
    store
        .set("pair:T-5:plan", json!({ "content": "# plan" }))
        .await;

    let input = json!({ "path": "src/main.rs" });
    let base = HookDecision::observe();
    let d = phase_guard(&store, "chat-5", "forge", "write", &input, base).await;
    assert!(d.deny, "an uploaded plan does not authorize source writes");
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

// ── Review fixes: plan-flag bypass, blocked resume, shell write evasion ──

#[tokio::test]
async fn phase_guard_denies_is_plan_marked_source_before_plan() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-30", "forge", "chat-30").await;
    seed_status(&store, "T-30", "planning").await;

    // A caller-supplied `is_plan` marker must not let a write to a source path
    // masquerade as a plan write (P1: "Plan flag bypasses gate").
    let input = json!({ "path": "src/lib.rs", "is_plan": true });
    let d = phase_guard(
        &store,
        "chat-30",
        "forge",
        "write",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        d.deny,
        "is_plan:true must not whitelist a source path before a plan exists"
    );
}

#[tokio::test]
async fn phase_guard_allows_is_plan_marked_plan_path() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-301", "forge", "chat-301").await;
    seed_status(&store, "T-301", "planning").await;

    // Writing the actual plan artifact remains allowed even with the flag set.
    let input = json!({ "path": "/home/coder/.coder/plans/PLAN-chat-301.md", "is_plan": true });
    let d = phase_guard(
        &store,
        "chat-301",
        "forge",
        "write",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        !d.deny,
        "writing the standard plan stays allowed in planning"
    );
}

#[tokio::test]
async fn phase_guard_denies_blocked_worker_advancing_to_building() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-31", "forge", "chat-31").await;
    seed_status(&store, "T-31", "blocked").await;

    // A blocked worker must not transition forward (e.g. building) on its own.
    let input = json!({ "command": "openflows-harness status set building" });
    let d = phase_guard(
        &store,
        "chat-31",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        d.deny,
        "blocked worker cannot resume building without authorization"
    );
}

#[tokio::test]
async fn phase_guard_allows_blocked_worker_return_to_planning() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-32", "forge", "chat-32").await;
    seed_status(&store, "T-32", "blocked").await;

    let input = json!({ "command": "openflows-harness status set planning" });
    let d = phase_guard(
        &store,
        "chat-32",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        !d.deny,
        "returning to planning is an acceptable blocked escape"
    );
}

#[tokio::test]
async fn phase_guard_allows_blocked_worker_blocker_report() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-33", "forge", "chat-33").await;
    seed_status(&store, "T-33", "blocked").await;

    let input = json!({ "path": "status.json" });
    let d = phase_guard(
        &store,
        "chat-33",
        "forge",
        "write",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(!d.deny, "blocker report writes are allowed while blocked");
}

#[tokio::test]
async fn phase_guard_denies_blocked_worker_building_shell_write() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-331", "forge", "chat-331").await;
    seed_status(&store, "T-331", "blocked").await;

    // Even a shell source write that slips past operand parsing must stay denied
    // in the blocked phase (P1: "Shell writes evade guard").
    let input = json!({ "command": "dd of=src/lib.rs" });
    let d = phase_guard(
        &store,
        "chat-331",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        d.deny,
        "blocked worker cannot write source via unknown shell"
    );
}

#[tokio::test]
async fn phase_guard_denies_blocked_worker_compound_probe_write() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-332", "forge", "chat-332").await;
    seed_status(&store, "T-332", "blocked").await;

    // A probe substring (git status) followed by a write must NOT classify the
    // whole command as a read-only probe (P1: "Blocked Probe Allows Writes").
    let input = json!({ "command": "git status; touch src/lib.rs" });
    let d = phase_guard(
        &store,
        "chat-332",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        d.deny,
        "compound probe+write command must be denied while blocked"
    );
}

#[tokio::test]
async fn phase_guard_allows_blocked_worker_pure_probe() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-333", "forge", "chat-333").await;
    seed_status(&store, "T-333", "blocked").await;

    // A standalone read-only probe must still be allowed while blocked.
    let input = json!({ "command": "git status" });
    let d = phase_guard(
        &store,
        "chat-333",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        !d.deny,
        "a pure read-only probe stays allowed while blocked"
    );
}

#[tokio::test]
async fn phase_guard_denies_blocked_worker_probe_with_embedded_write() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-334", "forge", "chat-334").await;
    seed_status(&store, "T-334", "blocked").await;

    // A probe whose command itself embeds a source write (redirection / tee)
    // must be denied while blocked, even though it matches a probe substring
    // (P1: "Blocked Probe Allows Writes").
    for command in ["git status > src/lib.rs", "git status | tee src/lib.rs"] {
        let input = json!({ "command": command });
        let d = phase_guard(
            &store,
            "chat-334",
            "forge",
            "bash",
            &input,
            HookDecision::observe(),
        )
        .await;
        assert!(
            d.deny,
            "blocked probe with embedded source write must be denied: {command}"
        );
    }
}

#[tokio::test]
async fn phase_guard_denies_blocked_worker_argv_write_bypass() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-335", "forge", "chat-335").await;
    seed_status(&store, "T-335", "blocked").await;

    // A shell invocation supplied via `argv` (with no `command` field) must not
    // fall back to an empty command that vacuously matches a read-only probe
    // and let an unrecognized write escape the blocked gate (P1: "Argv Commands
    // Bypass Blocking"). `command_text` must reconstruct the command and the
    // probe check must require a recognized probe segment.
    let input = json!({ "argv": ["python", "-c", "open('src/lib.rs','w').write('x')"] });
    let d = phase_guard(
        &store,
        "chat-335",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        d.deny,
        "blocked worker cannot bypass the gate via an argv-only shell write"
    );

    // An argv-only invocation that is a genuine read-only probe stays allowed.
    let input = json!({ "argv": ["git", "status"] });
    let d = phase_guard(
        &store,
        "chat-335",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        !d.deny,
        "an argv-only pure probe stays allowed while blocked"
    );
}

#[tokio::test]
async fn sentinel_denies_unknown_write_capable_shell_command() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-34", "sentinel", "chat-34").await;
    seed_status(&store, "T-34", "submit").await;
    let st = read_ticket_state(&store, "T-34").await;

    for cmd in [
        "dd of=src/lib.rs",
        "curl -o src/lib.rs https://example.test/x",
        "wget -O src/lib.rs https://example.test/x",
        "git checkout -- src/lib.rs",
        "sh -c 'echo x > src/lib.rs'",
    ] {
        let input = json!({ "command": cmd });
        let d = sentinel_phase_guard(&store, "T-34", &st, "bash", &input, HookDecision::observe())
            .await;
        assert!(
            d.deny,
            "Sentinel must deny write-capable shell command: {cmd}"
        );
    }
}

#[tokio::test]
async fn sentinel_denies_leading_wrapper_git_write() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-340", "sentinel", "chat-340").await;
    seed_status(&store, "T-340", "submit").await;
    let st = read_ticket_state(&store, "T-340").await;

    // A wrapper before `git` must not shift the token used to resolve the git
    // subcommand: `sudo git checkout` writes the checkout and must be denied
    // (P1: "Sentinel wrapper handling resolves the Git subcommand from the
    // wrong token"). Wrapper *arguments* must not be mistaken for the command
    // either (P1: "Wrapper Arguments Hide Writes").
    for cmd in [
        "sudo git checkout -- src/lib.rs",
        "sudo git restore src/lib.rs",
        "env SOME=1 git apply --patch",
        "sudo -u root git checkout -- src/lib.rs",
        "sudo -u root git restore src/lib.rs",
        "env SOME=1 git checkout src/lib.rs",
        "nice -n 10 git reset --hard HEAD",
    ] {
        let input = json!({ "command": cmd });
        let d = sentinel_phase_guard(
            &store,
            "T-340",
            &st,
            "bash",
            &input,
            HookDecision::observe(),
        )
        .await;
        assert!(
            d.deny,
            "Sentinel must deny git write with a leading wrapper: {cmd}"
        );
    }
}

#[tokio::test]
async fn phase_guard_denies_write_capable_shell_during_review_ready() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-35", "forge", "chat-35").await;
    seed_status(&store, "T-35", "submit").await;

    let input = json!({ "command": "curl -o src/lib.rs https://example.test/x" });
    let d = phase_guard(
        &store,
        "chat-35",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(
        d.deny,
        "review_ready must not allow source changes via unknown shell writes"
    );
}

#[tokio::test]
async fn phase_guard_denies_wrapped_git_checkout_write_capable() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-36", "forge", "chat-36").await;
    seed_status(&store, "T-36", "submit").await;

    // A wrapper (`sudo`) must not let the git subcommand resolve to the wrong
    // token and slip past write detection (P1: "Wrapped Commands Evade
    // Detection"). Wrapper *arguments* must not be mistaken for the command
    // (P1: "Wrapper Arguments Hide Writes").
    for cmd in [
        "sudo git checkout -- src/lib.rs",
        "env git restore src/lib.rs",
        "nohup git reset --hard HEAD",
        "sudo -u root git checkout -- src/lib.rs",
        "env SOME=1 git checkout src/lib.rs",
        "nice -n 10 git reset --hard HEAD",
    ] {
        let input = json!({ "command": cmd });
        let d = phase_guard(
            &store,
            "chat-36",
            "forge",
            "bash",
            &input,
            HookDecision::observe(),
        )
        .await;
        assert!(
            d.deny,
            "review_ready must deny wrapper-invoked git write: {cmd}"
        );
    }
}

#[tokio::test]
async fn phase_guard_denies_write_hidden_in_later_shell_segment() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-37", "forge", "chat-37").await;
    seed_status(&store, "T-37", "submit").await;

    // A write in a later command segment (after a shell operator) must still be
    // detected (P1: "Wrapped Commands Evade Detection").
    for cmd in [
        "true && dd of=src/lib.rs",
        "echo x ; curl -o src/lib.rs https://example.test/x",
        "ls | wget -O src/lib.rs https://example.test/x",
    ] {
        let input = json!({ "command": cmd });
        let d = phase_guard(
            &store,
            "chat-37",
            "forge",
            "bash",
            &input,
            HookDecision::observe(),
        )
        .await;
        assert!(
            d.deny,
            "review_ready must deny write hidden in later segment: {cmd}"
        );
    }
}

#[tokio::test]
async fn phase_guard_denies_compound_replan_bypassing_blocked_gate() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-38", "forge", "chat-38").await;
    seed_status(&store, "T-38", "blocked").await;

    // Merely *containing* `status set planning` must not let a compound
    // invocation escape the blocked gate with an unauthorized trailing write
    // (P1: "Compound Replans Bypass Blocking").
    for cmd in [
        "openflows-harness status set planning && dd of=src/lib.rs",
        "openflows-harness status set planning ; git checkout -- src/lib.rs",
    ] {
        let input = json!({ "command": cmd });
        let d = phase_guard(
            &store,
            "chat-38",
            "forge",
            "bash",
            &input,
            HookDecision::observe(),
        )
        .await;
        assert!(
            d.deny,
            "blocked compound command must not bypass the gate: {cmd}"
        );
    }
}

#[tokio::test]
async fn phase_guard_allows_exact_replan_from_blocked() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-39", "forge", "chat-39").await;
    seed_status(&store, "T-39", "blocked").await;

    // An exact return-to-planning transition remains a valid blocked escape,
    // even on the shell tool.
    let input = json!({ "command": "openflows-harness status set planning" });
    let d = phase_guard(
        &store,
        "chat-39",
        "forge",
        "bash",
        &input,
        HookDecision::observe(),
    )
    .await;
    assert!(!d.deny, "exact replan from blocked stays allowed");
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
    seed_status(&store, "T-9", "submit").await;
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
        skills_by_role: Default::default(),
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
        "openflows-harness plan write --file /home/coder/.coder/plans/PLAN-chat-18.md",
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
    assert!(is_valid_phase("submit"));
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
    let mut lifecycle = config::lifecycle::Lifecycle {
        phase: config::lifecycle::Phase::parse(phase).unwrap(),
        plan: if plan { "# plan".into() } else { String::new() },
        revision: 1,
        ..Default::default()
    };
    if gate {
        lifecycle.plan_decision = Some(config::lifecycle::Decision {
            pr_number: None,
            round: 0,
            actor: "sentinel".into(),
            approved: true,
            report: "ok".into(),
            revision: 1,
            head: None,
        });
    }
    store
        .set(
            &format!("ticket:{ticket}:status"),
            serde_json::to_value(lifecycle).unwrap(),
        )
        .await;
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

    seed_ticket_state(&store, "T-13", "plan_ready", true, false, false).await;
    assert_eq!(
        sentinel_job(&read_ticket_state(&store, "T-13").await),
        SentinelJob::PlanGateReview
    );

    seed_ticket_state(&store, "T-13", "submit", true, true, true).await;
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
    seed_ticket_state(&store, "T-14", "plan_ready", false, false, false).await;
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
    seed_ticket_state(&store, "T-15", "submit", true, true, true).await;
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

    let shell_source = json!({ "command": "printf 'oops' > src/main.rs" });
    let d3 = sentinel_phase_guard(
        &store,
        "T-15",
        &st,
        "Bash",
        &shell_source,
        HookDecision::observe(),
    )
    .await;
    assert!(d3.deny, "sentinel shell writes to source are blocked");

    let shell_copy = json!({ "command": "cp final-review.md src/main.rs" });
    let d4 = sentinel_phase_guard(
        &store,
        "T-15",
        &st,
        "Bash",
        &shell_copy,
        HookDecision::observe(),
    )
    .await;
    assert!(
        d4.deny,
        "review artifact source operands must not hide source write targets"
    );

    let report_redirect = json!({ "command": "printf 'ok' > final-review.md" });
    let d5 = sentinel_phase_guard(
        &store,
        "T-15",
        &st,
        "Bash",
        &report_redirect,
        HookDecision::observe(),
    )
    .await;
    assert!(!d5.deny, "sentinel may write review report artifacts");
}

#[tokio::test]
async fn sentinel_review_submit_requires_report_first() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-16", "sentinel", "chat-16").await;
    seed_ticket_state(&store, "T-16", "submit", true, true, true).await;

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

#[tokio::test]
async fn legacy_building_does_not_authorize_edits_before_new_review() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-legacy", "forge", "legacy-chat").await;
    store
        .set("ticket:T-legacy:status", json!({"phase":"building"}))
        .await;
    store.set("pair:T-legacy:plan", json!("old plan")).await;
    let result = phase_guard(
        &store,
        "legacy-chat",
        "forge",
        "write",
        &json!({"path":"src/lib.rs"}),
        HookDecision::observe(),
    )
    .await;
    assert!(result.deny);
}

#[tokio::test]
async fn planning_allows_grounded_readonly_reconnaissance() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-read", "forge", "chat-read").await;
    for phase in ["planning", "plan_ready", "plan_rejected"] {
        seed_status(&store, "T-read", phase).await;
        for command in [
            "cd /home/coder && ls -la && cat AGENTS.md 2>/dev/null | head -100",
            "cd /home/coder && openflows-harness status get 2>&1; openflows-harness dispatch read 2>&1 | head -100",
            "cd /home/coder/workspace && pwd && ls -la && rg --files",
            "rg -n marketplace src 2>/dev/null",
            "git status --short && git log --oneline -10",
            "docker info 2>&1 | head -30",
            "docker compose config --services 2>/dev/null",
            "cat src/main.rs >/dev/null 2>&1",
            "cat src/main.rs 2> /dev/null",
            "cat src/main.rs 1>&2",
        ] {
            let decision = phase_guard(&store, "chat-read", "forge", "execute",
                &json!({"command": command}), HookDecision::observe()).await;
            assert!(!decision.deny, "{phase} must allow inspection: {command}");
        }
        let decision = phase_guard(
            &store,
            "chat-read",
            "forge",
            "read_file",
            &json!({"path": "src/main.rs"}),
            HookDecision::observe(),
        )
        .await;
        assert!(!decision.deny);
    }
}

#[tokio::test]
async fn planning_reconnaissance_does_not_allow_hidden_source_writes() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-write", "forge", "chat-write").await;
    seed_status(&store, "T-write", "planning").await;
    for command in [
        "cat src/main.rs 2>/dev/null; touch src/new.rs",
        "git checkout -- src/main.rs >/dev/null 2>&1",
        "dd of=src/main.rs 2>/dev/null",
        r#"python3 -c 'open("src/main.rs", "w").write("bad")' >/dev/null"#,
        "cat README.md 2>&1 > src/main.rs",
        "cat README.md 2> src/main.rs",
        "cat README.md | tee src/main.rs >/dev/null",
        "printf bad > src/main.rs && cat README.md 2>&1",
    ] {
        let decision = phase_guard(
            &store,
            "chat-write",
            "forge",
            "execute",
            &json!({"command": command}),
            HookDecision::observe(),
        )
        .await;
        assert!(decision.deny, "must retain source edit gate: {command}");
    }
    for tool in ["write_file", "edit_file", "apply_patch"] {
        let decision = phase_guard(
            &store,
            "chat-write",
            "forge",
            tool,
            &json!({"path": "src/main.rs", "content": "bad"}),
            HookDecision::observe(),
        )
        .await;
        assert!(decision.deny, "must gate native source edits: {tool}");
    }
}

#[tokio::test]
async fn forge_bootstrap_uses_current_phase_instead_of_restarting_planning() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-resume", "forge", "chat-resume").await;
    let context = HookBootstrapContext::default();
    for phase in ["planning", "building", "blocked", "testing", "submit"] {
        seed_status(&store, "T-resume", phase).await;
        if phase != "planning" {
            store
                .set("pair:T-resume:plan", json!("# Approved plan"))
                .await;
        }
        let text = build_bootstrap_context(&store, "chat-resume", &json!({}), &context)
            .await
            .unwrap();
        if phase == "planning" {
            assert!(
                text.contains("read source"),
                "planning must start with investigation"
            );
        } else {
            assert!(
                !text.contains("Your first action: run `/plan`"),
                "resume in {phase} must not restart planning"
            );
        }
    }
}

#[tokio::test]
async fn chat_plan_path_is_writable_during_planning_and_frozen_during_review() {
    let store = SharedStore::new_in_memory();
    let chat = "41b12322-5719-4822-87e9-c01e576b49e4";
    seed_chat(&store, "T-plan-path", "forge", chat).await;
    let path = "/home/coder/.coder/plans/PLAN-41b12322-5719-4822-87e9-c01e576b49e4.md";
    for phase in [
        "planning",
        "plan_rejected",
        "plan_ready",
        "testing",
        "submit",
        "blocked",
    ] {
        seed_status(&store, "T-plan-path", phase).await;
        for (tool, input) in [
            ("write_file", json!({"path": path, "content": "# Plan"})),
            ("edit", json!({"file_path": path})),
            (
                "bash",
                json!({"command": format!("printf '# Plan' > {path}")}),
            ),
        ] {
            let decision =
                phase_guard(&store, chat, "forge", tool, &input, HookDecision::observe()).await;
            assert_eq!(
                decision.deny,
                !matches!(phase, "planning" | "plan_rejected"),
                "{phase} {tool}: {:?}",
                decision.reason
            );
        }
    }
}

#[tokio::test]
async fn chat_plan_path_does_not_allow_other_chats_or_source_writes() {
    let store = SharedStore::new_in_memory();
    let chat = "41b12322-5719-4822-87e9-c01e576b49e4";
    seed_chat(&store, "T-plan-scope", "forge", chat).await;
    seed_status(&store, "T-plan-scope", "planning").await;
    for path in [
        "/home/coder/.coder/plans/PLAN-other-chat.md",
        "/home/coder/workspace/src/PLAN-41b12322-5719-4822-87e9-c01e576b49e4.md",
        "/home/coder/.coder/plans/../PLAN-41b12322-5719-4822-87e9-c01e576b49e4.md",
    ] {
        for (tool, input) in [
            ("write", json!({"path": path})),
            ("bash", json!({"command": format!("printf plan > {path}")})),
        ] {
            assert!(
                phase_guard(&store, chat, "forge", tool, &input, HookDecision::observe())
                    .await
                    .deny,
                "{tool} {path}"
            );
        }
    }
    let command =
        format!("printf plan > /home/coder/.coder/plans/PLAN-{chat}.md; touch src/main.rs");
    assert!(
        phase_guard(
            &store,
            chat,
            "forge",
            "bash",
            &json!({"command": command}),
            HookDecision::observe()
        )
        .await
        .deny
    );
}

#[tokio::test]
async fn chat_plan_path_is_provided_in_startup_and_denial_guidance() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-plan-help", "forge", "chat-plan-help").await;
    seed_status(&store, "T-plan-help", "planning").await;
    let expected = "/home/coder/.coder/plans/PLAN-chat-plan-help.md";
    let context = build_bootstrap_context(
        &store,
        "chat-plan-help",
        &json!({}),
        &HookBootstrapContext::default(),
    )
    .await
    .unwrap();
    assert!(context.contains(expected));
    let decision = phase_guard(
        &store,
        "chat-plan-help",
        "forge",
        "write",
        &json!({"path":"src/main.rs"}),
        HookDecision::observe(),
    )
    .await;
    assert!(decision.reason.unwrap().contains(expected));
}

#[tokio::test]
async fn standard_plan_rejects_legacy_paths_for_file_and_shell_writes() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-standard", "forge", "standard-chat").await;
    seed_status(&store, "T-standard", "planning").await;
    for path in [
        "PLAN.md",
        "plan",
        "/home/coder/PLAN.md",
        "/home/coder/workspace/PLAN.md",
        "src/PLAN.md",
    ] {
        for (tool, input) in [
            ("write", json!({"path":path})),
            ("bash", json!({"command":format!("printf plan > {path}")})),
        ] {
            let decision = phase_guard(
                &store,
                "standard-chat",
                "forge",
                tool,
                &input,
                HookDecision::observe(),
            )
            .await;
            assert!(decision.deny, "legacy plan path allowed: {tool} {path}");
        }
    }
}

#[tokio::test]
async fn standard_plan_upload_requires_current_chat_path() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-upload", "forge", "upload-chat").await;
    seed_status(&store, "T-upload", "planning").await;
    for (args, allowed) in [
        ("--file /home/coder/.coder/plans/PLAN-upload-chat.md", true),
        ("--file=/home/coder/.coder/plans/PLAN-upload-chat.md", true),
        ("--file PLAN.md", false),
        ("--file /home/coder/.coder/plans/PLAN-other-chat.md", false),
        ("--file src/main.rs", false),
    ] {
        for (tool, prefix) in [("bash", "openflows-harness "), ("openflows-harness", "")] {
            let input = json!({"command":format!("{prefix}plan write {args}")});
            let decision = phase_guard(
                &store,
                "upload-chat",
                "forge",
                tool,
                &input,
                HookDecision::observe(),
            )
            .await;
            assert_eq!(!decision.deny, allowed, "{tool} {args}");
        }
    }
}

#[tokio::test]
async fn standard_plan_checks_uploads_after_shell_command_boundaries() {
    let store = SharedStore::new_in_memory();
    seed_chat(&store, "T-upload-boundary", "forge", "upload-chat").await;
    seed_status(&store, "T-upload-boundary", "planning").await;
    for separator in ["\n", "&&", ";", " || "] {
        let command = format!("openflows-harness status get{separator}openflows-harness plan write --file /home/coder/PLAN.md");
        let decision = phase_guard(
            &store,
            "upload-chat",
            "forge",
            "bash",
            &json!({"command":command}),
            HookDecision::observe(),
        )
        .await;
        assert!(
            decision.deny,
            "legacy upload after separator {separator:?} must be rejected"
        );
    }
}

#[tokio::test]
async fn sandbox_delegation_allows_literal_tooling_without_host_shell_bypass() {
    let store = SharedStore::new_in_memory();
    seed_status(&store, "T-sandbox", "testing").await;
    let state = read_ticket_state(&store, "T-sandbox").await;
    for command in [
        "openflows-harness verify request --expect-exit 0 -- pytest -q",
        "openflows-harness verify request --expect-exit 0 -- python3 -c 'print(1); print(2)'",
        "openflows-harness verify request --expect-exit 0 -- ./scripts/custom-test",
    ] {
        assert!(super::guard::is_verification_request(command));
        let d = sentinel_phase_guard(
            &store,
            "T-sandbox",
            &state,
            "bash",
            &json!({"command":command}),
            HookDecision::observe(),
        )
        .await;
        assert!(!d.deny, "{command}");
    }
    for command in [
        "openflows-harness verify request -- pytest; touch src/main.rs",
        "openflows-harness verify request -- pytest > src/main.rs",
        "openflows-harness verify request -- echo $(touch src/main.rs)",
        "openflows-harness verify request -- echo \"$(touch src/main.rs)\"",
    ] {
        assert!(!super::guard::is_verification_request(command), "{command}");
    }
}
