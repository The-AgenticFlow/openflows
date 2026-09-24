//! Models for fleet status and runtime state APIs.

use config::state::{HeartbeatRecord, WorkerSlot};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Aggregated counts of tickets grouped by status.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct TicketCounts {
    pub total: usize,
    pub open: usize,
    pub assigned: usize,
    pub in_progress: usize,
    pub merged: usize,
    pub failed: usize,
    pub completed: usize,
    pub exhausted: usize,
    pub awaiting_human: usize,
}

/// A single ticket that requires human attention or failed permanently.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct EscalatedTicketSummary {
    pub id: String,
    pub title: String,
    pub reason: String,
    pub attempts: u32,
    pub escalation_type: String, // "awaiting_human" or "failed"
}

/// Summary of escalations within a tenant fleet.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq, Default)]
pub struct EscalationSummary {
    pub awaiting_human_count: usize,
    pub failed_count: usize,
    pub tickets: Vec<EscalatedTicketSummary>,
}

/// Fine-grained workflow phase status (`ticket:{id}:status`).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct PhaseStatus {
    pub phase: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub role: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ts: Option<u64>,
}

/// Single tenant fleet runtime snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantFleetResponse {
    pub tenant: String,
    pub repository: Option<String>,
    pub ticket_counts: TicketCounts,
    pub tickets: Vec<serde_json::Value>,
    #[serde(default)]
    pub phases: HashMap<String, PhaseStatus>,
    #[serde(default)]
    pub worker_slots: HashMap<String, WorkerSlot>,
    #[serde(default)]
    pub pending_prs: Vec<serde_json::Value>,
    pub ci_readiness: Option<String>,
    #[serde(default)]
    pub heartbeats: HashMap<String, HeartbeatRecord>,
    pub escalations: EscalationSummary,
}

/// High-level totals across all tenants.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct FleetOverviewSummary {
    pub total_tickets: usize,
    pub total_active_workers: usize,
    pub total_pending_prs: usize,
    pub total_escalations: usize,
}

/// Multi-tenant fleet response.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FleetSummaryResponse {
    pub total_tenants: usize,
    pub tenants: Vec<TenantFleetResponse>,
    pub summary: FleetOverviewSummary,
}
