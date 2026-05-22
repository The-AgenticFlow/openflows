use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

// ── Channel-agnostic message types ──────────────────────────────────────────

/// An inbound message from any channel plugin.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct InboundMessage {
    pub message_id: String,
    pub channel_id: String,
    pub user_id: String,
    pub conversation_id: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub metadata: serde_json::Value,
}

/// An outbound message to be routed to channel plugins.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OutboundMessage {
    pub message_type: OutboundMessageType,
    pub target_channel: Option<String>,
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
    PrOpened,
    PrMerged,
    CiFailed,
    CiTimeout,
    CiMissing,
    MergeBlocked,
    ConflictsDetected,
    TicketFailed,
    TicketExhausted,
    FuelExhausted,
    WorkerSuspended,
    HumanIntervention,
}

// ── SystemCommand (replaces HumanCommand) ───────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SystemCommand {
    PauseWorkflow {
        ticket_id: String,
    },
    ResumeWorkflow {
        ticket_id: String,
    },
    ApproveCommand {
        worker_id: String,
    },
    BlockAgent {
        worker_id: String,
        reason: String,
    },
    RerouteAgent {
        from_worker: String,
        to_worker: String,
    },
    AnswerQuestion {
        ticket_id: String,
        answer: String,
    },
    StatusQuery,
    GeneralMessage {
        text: String,
    },
}

#[derive(Debug, Clone)]
pub struct InterpretedCommand {
    pub command: SystemCommand,
    pub source: InboundMessage,
    pub confidence: f32,
}
