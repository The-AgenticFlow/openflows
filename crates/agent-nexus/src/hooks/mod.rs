// crates/agent-nexus/src/hooks/mod.rs
//! Coder Agent Lifecycle Hooks — OpenFlows webhook consumer (experimental).
//!
//! Mirrors how Coder's `agent-lifecycle-hooks` experiment works (see
//! coder/coder `docs/admin/setup/chat-lifecycle-hooks.md`). Coder `chatd`
//! POSTs a JWT-signed lifecycle event to one deployment-wide webhook URL
//! (`CODER_CHAT_HOOK_URL`) for each event; the consumer owns the audit trail.
//!
//! OpenFlows hosts this consumer inside the Controller (alongside the A2A
//! relay). It verifies the HS256 JWT, observes events centrally, persists a
//! durable log, and provides the seam to deny/rewrite `user_prompt_submit` /
//! `pre_tool_use` — the centralized analogue of the in-workspace
//! `orchestration/plugin/hooks/{role}/` shell hooks.
//!
//! Enablement (Coder side, deployment config):
//!   CODER_EXPERIMENTS=agent-lifecycle-hooks
//!   CODER_CHAT_HOOK_URL=http://<host>:3001/hooks/chat
//!   CODER_CHAT_HOOK_SECRET=<>=32 random bytes>
//! The OpenFlows consumer binds on `OPENFLOWS_HOOK_ADDR` (default 127.0.0.1:3001).

mod bootstrap;
mod context;
mod guard;
mod jwt;
mod kick;
mod server;
mod simulate;
mod stop;
mod types;

pub use bootstrap::{build_bootstrap_context, HookBootstrapContext};
pub use context::{read_ticket_state, resolve_chat, HookContext, TicketState};
pub use guard::phase_guard;
pub use jwt::verify_hook_jwt;
pub use kick::maybe_publish_kick;
pub use server::{create_router, start_lifecycle_hook_server};
pub use simulate::dispatch_simulated_event;
pub use stop::{classify_stop, stop_audit_record, StopClassification};
pub use types::{HookDecision, HookEvent, HookPayload};

#[cfg(test)]
mod tests;
#[cfg(test)]
mod slice_tests;
