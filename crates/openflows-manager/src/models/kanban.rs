//! Models for normalized Kanban and ticket detail APIs.

use crate::models::fleet::PhaseStatus;
use serde::{Deserialize, Serialize};

/// Canonical product-facing Kanban stage derived from TicketStatus + phase.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KanbanStage {
    Open,
    Planning,
    Building,
    Testing,
    Review,
    AwaitingHuman,
    Done,
    Failed,
}

impl KanbanStage {
    pub fn label(&self) -> &'static str {
        match self {
            Self::Open => "Open",
            Self::Planning => "Planning",
            Self::Building => "Building",
            Self::Testing => "Testing",
            Self::Review => "Review",
            Self::AwaitingHuman => "Awaiting Human",
            Self::Done => "Done",
            Self::Failed => "Failed",
        }
    }
}

/// Normalized ticket summary suitable for Kanban boards.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedTicket {
    pub id: String,
    pub title: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub body: Option<String>,
    pub priority: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issue_url: Option<String>,
    pub attempts: u32,
    pub raw_status: serde_json::Value,
    pub status_type: String,
    pub stage: KanbanStage,
    pub stage_label: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub assigned_worker: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub phase: Option<PhaseStatus>,
}

/// A Sentinel/agent review payload on a ticket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketReviewRecord {
    pub role: String,
    pub payload: serde_json::Value,
}

/// A gate approval payload on a ticket.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TicketGateRecord {
    pub phase: String,
    pub payload: serde_json::Value,
}

/// Detailed normalized ticket view with all workflow artifacts.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NormalizedTicketDetail {
    #[serde(flatten)]
    pub ticket: NormalizedTicket,
    pub tenant: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pr: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pending_pr: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub review: Option<serde_json::Value>,
    pub reviews: Vec<TicketReviewRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub gate: Option<serde_json::Value>,
    pub gates: Vec<TicketGateRecord>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub handoff: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub deployment: Option<serde_json::Value>,
}
