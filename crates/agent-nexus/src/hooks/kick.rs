// crates/agent-nexus/src/hooks/kick.rs
//! Slice B — reactive wake-up on `post_tool_use` / `stop`.
//!
//! A state-mutating harness call inside an agent workspace writes durable state
//! (phase, verdict, gate, PR, handoff) and then Coder fires `post_tool_use`. We
//! publish a kick so the Controller re-runs its reconciliation pass immediately
//! instead of waiting the full poll interval — e.g. a sentinel verdict lands and
//! Nexus resumes the paused forge right away.
//!
//! The kick is a wake-you-up, never a state write: correctness still lives in
//! the durable keys the harness wrote; a lost kick is just latency.

use super::context::resolve_chat;
use crate::hooks::types::HookEvent;
use pocketflow_core::{HookKick, HookKickPublisher};
use pocketflow_core::SharedStore;
use serde_json::Value;

/// Harness coordination substrings that mutate durable orchestration state and
/// therefore warrant waking the Controller.
const MUTATING_HINTS: &[(&str, &str)] = &[
    ("status set", "phase_changed"),
    ("review submit", "verdict_written"),
    ("gate approve", "gate_approved"),
    ("pr opened", "pr_submitted"),
    ("handoff write", "handoff_written"),
    ("plan write", "plan_written"),
    ("merge done", "merged"),
];

/// Derive a short "why" hint for a harness command (or `None` if not a
/// state-mutating coordination call).
fn mutating_hint(command: &str) -> Option<&'static str> {
    let cmd = command.to_lowercase();
    MUTATING_HINTS
        .iter()
        .find(|(key, _)| cmd.contains(key))
        .map(|(_, hint)| *hint)
}

/// Decide whether a `post_tool_use` dispatch should publish a kick, and build
/// the kick payload.
pub async fn maybe_publish_kick(
    publisher: &HookKickPublisher,
    store: &SharedStore,
    chat_id: &str,
    dispatch_id: &str,
    event: HookEvent,
    data: &Value,
) {
    let tool_name = data
        .get("tool_name")
        .and_then(|v| v.as_str())
        .unwrap_or_default()
        .to_lowercase();

    // Only harness / coordination tools carry durable state mutations.
    let is_harness = tool_name.contains("harness")
        || tool_name == "bash"
        || tool_name == "sh"
        || tool_name == "shell"
        || tool_name == "exec";
    if !is_harness {
        return;
    }

    let command = data
        .get("tool_input")
        .and_then(|i| i.get("command"))
        .and_then(|c| c.as_str())
        .unwrap_or_default();

    let hint = match mutating_hint(command) {
        Some(h) => h,
        None => return,
    };

    let hc = resolve_chat(store, chat_id).await;

    let mut kick = HookKick::new(chat_id, event_name(event));
    kick.dispatch_id = dispatch_id.to_string();
    kick.role = hc.role.unwrap_or_default();
    kick.ticket_id = hc.ticket_id.unwrap_or_default();
    kick.hint = hint.to_string();
    kick.data = json_extract_stdout(data);

    publisher.publish(&kick).await;
}

/// Capture the (trimmed) stdout of the tool response as ad-hoc kick data.
fn json_extract_stdout(data: &Value) -> Value {
    data.get("tool_response")
        .and_then(|r| r.get("stdout"))
        .cloned()
        .unwrap_or(Value::Null)
}

fn event_name(event: HookEvent) -> String {
    match event {
        HookEvent::SessionStart => "session_start".to_string(),
        HookEvent::UserPromptSubmit => "user_prompt_submit".to_string(),
        HookEvent::PreToolUse => "pre_tool_use".to_string(),
        HookEvent::PostToolUse => "post_tool_use".to_string(),
        HookEvent::PreCompact => "pre_compact".to_string(),
        HookEvent::PostCompact => "post_compact".to_string(),
        HookEvent::Stop => "stop".to_string(),
    }
}
