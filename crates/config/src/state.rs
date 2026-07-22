// crates/config/src/state.rs
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Ticket {
    pub id: String,
    pub title: String,
    pub body: String,
    pub priority: u32,
    pub branch: Option<String>,
    #[serde(default)]
    pub status: TicketStatus,
    #[serde(default)]
    pub issue_url: Option<String>,
    #[serde(default)]
    pub attempts: u32,
}

impl Ticket {
    pub const MAX_ATTEMPTS: u32 = 3;

    pub fn is_assignable(&self) -> bool {
        match &self.status {
            TicketStatus::Open => true,
            TicketStatus::Failed { attempts, .. } => *attempts < Self::MAX_ATTEMPTS,
            _ => false,
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum TicketStatus {
    #[serde(rename = "open")]
    #[default]
    Open,
    #[serde(rename = "assigned")]
    Assigned { worker_id: String },
    #[serde(rename = "in_progress")]
    InProgress { worker_id: String },
    #[serde(rename = "merged")]
    Merged { worker_id: String, pr_number: u64 },
    #[serde(rename = "failed")]
    Failed {
        worker_id: String,
        reason: String,
        attempts: u32,
    },
    #[serde(rename = "completed")]
    Completed { worker_id: String, outcome: String },
    #[serde(rename = "exhausted")]
    Exhausted { worker_id: String, attempts: u32 },
    #[serde(rename = "awaiting_human")]
    AwaitingHuman {
        worker_id: String,
        reason: String,
        attempts: u32,
    },
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkerSlot {
    pub id: String,
    pub status: WorkerStatus,
    #[serde(default)]
    pub workspace_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum WorkerStatus {
    Idle,
    Assigned {
        ticket_id: String,
        issue_url: Option<String>,
    },
    Working {
        ticket_id: String,
        issue_url: Option<String>,
    },
    Done {
        ticket_id: String,
        outcome: String,
    },
    Suspended {
        ticket_id: String,
        reason: String,
        issue_url: Option<String>,
    },
}

pub const KEY_TICKETS: &str = "tickets";
pub const KEY_WORKER_SLOTS: &str = "worker_slots";
pub const KEY_PENDING_PRS: &str = "pending_prs";
#[deprecated(note = "Use KEY_PENDING_PRS for clarity")]
pub const KEY_OPEN_PRS: &str = "open_prs";
pub const KEY_COMMAND_GATE: &str = "command_gate";
pub const KEY_DOCUMENTATION_QUEUE: &str = "documentation_queue";

pub const ACTION_WORK_ASSIGNED: &str = "work_assigned";
pub const ACTION_PR_OPENED: &str = "pr_opened";
pub const ACTION_NO_WORK: &str = "no_work";
pub const ACTION_EMPTY: &str = "empty";
pub const ACTION_FAILED: &str = "failed";
pub const ACTION_DEPLOYED: &str = "deployed";
pub const ACTION_DEPLOY_FAILED: &str = "deploy_failed";
pub const ACTION_MERGE_BLOCKED: &str = "merge_blocked";
pub const ACTION_MERGE_PRS: &str = "merge_prs";
pub const ACTION_CONFLICTS_DETECTED: &str = "conflicts_detected";
pub const ACTION_CI_FIX_NEEDED: &str = "ci_fix_needed";
pub const ACTION_DOCS_COMPLETE: &str = "docs_complete";
pub const ACTION_DOCS_PENDING: &str = "docs_pending";
pub const ACTION_AWAITING_HUMAN: &str = "awaiting_human";

// ── Ticket-scoped SharedStore key prefixes (Phase 5) ────────────────────

/// Build a ticket-scoped key like `ticket:T-42:chat:forge`.
pub fn ticket_key(ticket_id: &str, subkey: &str) -> String {
    format!("ticket:{}:{}", ticket_id, subkey)
}

/// Key suffix for the workspace ID assigned to a ticket + role.
/// Full key: `ticket:{id}:workspace:{role}`
pub const KEY_TICKET_WORKSPACE: &str = "workspace";

/// Key suffix for the task dispatch payload from Nexus to Forge.
/// Full key: `ticket:{id}:dispatch:{role}`
pub const KEY_TICKET_DISPATCH: &str = "dispatch";

/// Key suffix for the Coder Chat ID bound to a ticket + role.
/// Full key: `ticket:{id}:chat:{role}`
pub const KEY_TICKET_CHAT: &str = "chat";

/// Key suffix for Sentinel review results.
/// Full key: `ticket:{id}:review:{role}`
pub const KEY_TICKET_REVIEW: &str = "review";

/// Key suffix for vessel deployment status.
/// Full key: `ticket:{id}:deployment`
pub const KEY_TICKET_DEPLOYMENT: &str = "deployment";

/// Key suffix for current ticket status.
/// Full key: `ticket:{id}:status`
pub const KEY_TICKET_STATUS: &str = "status";

/// Key suffix for the last action Nexus took on a chat (for waiting disambiguation).
/// Full key: `ticket:{id}:chat_action:{role}`
pub const KEY_TICKET_CHAT_ACTION: &str = "chat_action";

/// Key suffix for recovery attempt counter.
/// Full key: `ticket:{id}:recovery_attempts`
pub const KEY_TICKET_RECOVERY_ATTEMPTS: &str = "recovery_attempts";

/// Key suffix for the diff_status payload from coder chats.
/// Full key: `ticket:{id}:diff_status:{role}`
pub const KEY_TICKET_DIFF_STATUS: &str = "diff_status";

/// Heartbeat key pattern: `heartbeat:{role}-T-{ticket_id}`
pub fn heartbeat_key(role: &str, ticket_id: &str) -> String {
    format!("heartbeat:{}-T-{}", role, ticket_id)
}

/// Heartbeat payload written by each workspace.
/// Stored as JSON under the `heartbeat:{role}-T-{ticket_id}` key.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HeartbeatRecord {
    pub ts: u64,
    pub ws_id: String,
    pub status: String,
}

/// Build a full ticket-scoped shared store key path.
/// e.g. `ticket:T-42:chat:forge`
pub fn full_ticket_key(ticket_id: &str, subkey: &str, role: &str) -> String {
    format!("ticket:{}:{}:{}", ticket_id, subkey, role)
}

/// Build a full ticket-scoped shared store key path without role suffix.
/// e.g. `ticket:T-42:status`
pub fn full_ticket_key_flat(ticket_id: &str, subkey: &str) -> String {
    format!("ticket:{}:{}", ticket_id, subkey)
}
