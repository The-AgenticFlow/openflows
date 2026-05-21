use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Channel-agnostic message types ──────────────────────────────────────────

/// An inbound message from any channel plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub message_id: String,
    pub channel_id: String,       // which plugin sent it ("slack", "discord", ...)
    pub user_id: String,
    pub conversation_id: String,  // channel/thread/group ID
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub metadata: serde_json::Value,  // channel-specific extras
}

/// An outbound message to be routed to channel plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub message_type: OutboundMessageType,
    pub target_channel: Option<String>,  // None = all channels
    pub target_conversation: Option<String>,
    pub content: String,
    pub ticket_id: Option<String>,
    pub worker_id: Option<String>,
    pub metadata: serde_json::Value,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum OutboundMessageType {
    // Original variants
    WorkflowStarted,
    AgentAssigned,
    AgentCompleted,
    WorkflowError,
    QuestionToHuman,
    StatusUpdate,
    ApprovalRequest,
    PauseWorkflow,
    ResumeWorkflow,
    AnswerQuestion,
    ApproveCommand,
    RerouteAgent,
    BlockAgent,
    // New variants for comprehensive stakeholder notifications
    PrOpened,            // FORGE opened a PR
    PrMerged,            // VESSEL merged a PR
    CiFailed,            // CI checks failed on a PR
    CiTimeout,           // CI polling timed out
    CiMissing,           // No CI workflows configured in repo
    MergeBlocked,        // GitHub API blocked the merge
    ConflictsDetected,   // Merge conflicts detected
    TicketFailed,        // Ticket marked as failed
    TicketExhausted,     // Ticket exceeded max attempts
    FuelExhausted,       // Agent ran out of fuel/time
    WorkerSuspended,     // Worker needs human approval
}

// ── SystemCommand (replaces HumanCommand) ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SystemCommand {
    PauseWorkflow { ticket_id: String },
    ResumeWorkflow { ticket_id: String },
    ApproveCommand { worker_id: String },
    BlockAgent { worker_id: String, reason: String },
    RerouteAgent { from_worker: String, to_worker: String },
    AnswerQuestion { ticket_id: String, answer: String },
    StatusQuery,
    GeneralMessage { text: String },
}

#[derive(Debug, Clone)]
pub struct InterpretedCommand {
    pub command: SystemCommand,
    pub source: InboundMessage,
    pub confidence: f32,
}
