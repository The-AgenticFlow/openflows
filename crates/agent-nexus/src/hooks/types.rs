// crates/agent-nexus/src/hooks/types.rs
//! Wire types for Coder's experimental `agent-lifecycle-hooks`.
//!
//! Mirrors `codersdk/x/agenthooks/types.go` event lifecycle. Coder `chatd`
//! POSTs one of these to the deployment-wide webhook URL at each lifecycle
//! event. A consumer may observe every event and may **deny or rewrite** the
//! mutable ones (`user_prompt_submit`, `pre_tool_use`).
//!
//! Event names match Claude Code hooks (session_start, pre_tool_use, ...); the
//! difference is transport: Coder-side hooks are server-side, deployment-wide
//! HTTP POSTs, whereas OpenFlows' existing `orchestration/plugin/hooks/{role}/`
//! are client-side shell scripts executed inside each agent workspace.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The lifecycle events Coder can dispatch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HookEvent {
    /// A chat starts, resumes, or clears. Not mutable.
    SessionStart,
    /// A user or `spawn_agent` submits a prompt. Mutable (may deny/rewrite).
    UserPromptSubmit,
    /// Before a tool runs. Mutable (may deny/rewrite).
    PreToolUse,
    /// After a tool returns. Not mutable.
    PostToolUse,
    /// Before context compaction. Not mutable.
    PreCompact,
    /// After context compaction. Not mutable.
    PostCompact,
    /// The model ends a turn. Not mutable.
    Stop,
}

impl HookEvent {
    /// Whether the consumer may return a deny/rewrite decision for this event.
    pub fn is_mutable(&self) -> bool {
        matches!(self, HookEvent::UserPromptSubmit | HookEvent::PreToolUse)
    }
}

/// The signed envelope Coder sends in the webhook request body.
///
/// Coder's real wire shape is `{ "type", "meta", "data" }`:
///   - `type`  — the lifecycle event name (mirrored in the JWT `type` claim),
///   - `meta`  — `dispatch_id`, `schema_version`, `chat_id`, `owner_id`,
///     optional `workspace_id` / `turn_id`, and for subagent chats
///     `parent_chat_id` / `root_chat_id`,
///   - `data`  — event-specific fields.
#[derive(Debug, Clone, Deserialize)]
pub struct HookPayload {
    /// Which lifecycle event fired (matches JWT `type` claim).
    #[serde(rename = "type")]
    pub event: HookEvent,
    /// Dispatch envelope (identifiers the JWT binds to).
    pub meta: HookMeta,
    /// Free-form event context (tool name/input for tool events, prompt body
    /// for user_prompt_submit, etc.).
    #[serde(default)]
    pub data: Value,
}

/// Identifiers Coder attaches to every dispatch (mirrors `meta`).
#[derive(Debug, Clone, Deserialize)]
pub struct HookMeta {
    /// ID of the dispatch (dedupe transport retries; must equal JWT `jti`).
    #[serde(default)]
    pub dispatch_id: String,
    /// Wire schema version (current `1`).
    #[serde(default)]
    pub schema_version: u32,
    /// Chat the event belongs to (also the `sub` suffix in the JWT).
    #[serde(default)]
    pub chat_id: String,
    /// Owner of the chat.
    #[serde(default)]
    pub owner_id: String,
    /// Optional workspace the event occurred in.
    #[serde(default)]
    pub workspace_id: Option<String>,
    /// Optional turn the event belongs to.
    #[serde(default)]
    pub turn_id: Option<String>,
    /// Present for subagent chats (correlate a subagent subtree).
    #[serde(default)]
    pub parent_chat_id: Option<String>,
    /// Present for subagent chats (root conversation).
    #[serde(default)]
    pub root_chat_id: Option<String>,
    /// Client label, when Coder provides one.
    #[serde(default)]
    pub client: Option<String>,
}

/// Consumer decision returned for mutable events. For non-mutable events the
/// body is ignored and the event is observed only.
#[derive(Debug, Clone, Serialize, Default)]
pub struct HookDecision {
    /// Whether to deny the action. Only honoured for `user_prompt_submit`
    /// and `pre_tool_use`.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub deny: bool,
    /// Optional human-readable reason surfaced for deny decisions.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    /// Optional rewritten context/message body returned to the agent loop.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rewrite: Option<Value>,
    /// Optional model-only context injected back into the agent loop. Capped
    /// at ~16 KiB (D9). Populated by `user_prompt_submit` (feed current
    /// status to the model before it answers) and by `post_tool_use` (feed
    /// back guidance/errors accumulated by `pre_tool_use` policy checks).
    /// Serialized as the top-level Coder `model_context` field, independent
    /// of `deny`/`rewrite`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_context: Option<String>,
}

impl HookDecision {
    /// An observation-only (no-op) decision.
    pub fn observe() -> Self {
        Self::default()
    }

    /// A denial decision with a reason.
    pub fn deny(reason: impl Into<String>) -> Self {
        Self {
            deny: true,
            reason: Some(reason.into()),
            rewrite: None,
            model_context: None,
        }
    }

    /// Attach model-only context to this decision (any deny/rewrite state is
    /// preserved). Used when the consumer wants to inject status/guidance into
    /// the model even for an otherwise-allowed action.
    pub fn with_model_context(mut self, context: impl Into<String>) -> Self {
        self.model_context = Some(context.into());
        self
    }
}
