//! Fleet status and runtime state service.

use crate::{
    error::ManagerError,
    models::fleet::{
        EscalatedTicketSummary, EscalationSummary, FleetOverviewSummary, FleetSummaryResponse,
        PhaseStatus, TenantFleetResponse, TicketCounts,
    },
    services::tenant::validate_tenant_name,
};
use config::state::{HeartbeatRecord, WorkerSlot};
use pocketflow_core::SharedStore;
use std::collections::{HashMap, HashSet};

/// Service for inspecting runtime fleet state.
#[derive(Clone)]
pub struct FleetService {
    store: SharedStore,
}

impl FleetService {
    pub fn new(store: SharedStore) -> Self {
        Self { store }
    }

    /// Retrieve fleet status and agent state across all tenants.
    pub async fn get_fleet_summary(&self) -> Result<FleetSummaryResponse, ManagerError> {
        let keys = self.store.raw_keys("ns:*").await;
        let mut tenant_names = HashSet::new();
        for key in keys {
            if let Some(ns) = key.strip_prefix("ns:") {
                if let Some(tenant) = ns.split(':').next() {
                    if !tenant.is_empty() {
                        tenant_names.insert(tenant.to_string());
                    }
                }
            }
        }

        let mut sorted_names: Vec<String> = tenant_names.into_iter().collect();
        sorted_names.sort();

        let mut tenants = Vec::with_capacity(sorted_names.len());
        let mut total_tickets = 0;
        let mut total_active_workers = 0;
        let mut total_pending_prs = 0;
        let mut total_escalations = 0;

        for name in sorted_names {
            if let Ok(tenant_fleet) = self.get_tenant_fleet(&name).await {
                total_tickets += tenant_fleet.ticket_counts.total;
                total_active_workers += tenant_fleet
                    .worker_slots
                    .values()
                    .filter(|slot| {
                        matches!(
                            slot.status,
                            config::state::WorkerStatus::Assigned { .. }
                                | config::state::WorkerStatus::Working { .. }
                        )
                    })
                    .count();
                total_pending_prs += tenant_fleet.pending_prs.len();
                total_escalations += tenant_fleet.escalations.awaiting_human_count
                    + tenant_fleet.escalations.failed_count;

                tenants.push(tenant_fleet);
            }
        }

        Ok(FleetSummaryResponse {
            total_tenants: tenants.len(),
            tenants,
            summary: FleetOverviewSummary {
                total_tickets,
                total_active_workers,
                total_pending_prs,
                total_escalations,
            },
        })
    }

    /// Retrieve runtime fleet state for a single tenant.
    pub async fn get_tenant_fleet(
        &self,
        tenant: &str,
    ) -> Result<TenantFleetResponse, ManagerError> {
        validate_tenant_name(tenant)?;

        let pattern = format!("ns:{}:*", tenant);
        let keys = self.store.raw_keys(&pattern).await;
        if keys.is_empty() {
            return Err(ManagerError::TenantNotFound(tenant.to_string()));
        }

        let t_store = self.store.for_tenant(tenant);
        let repository: Option<String> = t_store.get_typed("repository").await;
        let raw_tickets: Vec<serde_json::Value> =
            t_store.get_typed("tickets").await.unwrap_or_default();

        let mut counts = TicketCounts::default();
        let mut escalated_tickets = Vec::new();

        for t in &raw_tickets {
            counts.total += 1;
            let status_val = t.get("status");
            let stype = match status_val {
                Some(serde_json::Value::Object(obj)) => {
                    obj.get("type").and_then(|v| v.as_str()).unwrap_or("")
                }
                Some(serde_json::Value::String(s)) => s.as_str(),
                _ => "",
            };

            let ticket_id = t
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let title = t
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let attempts = t.get("attempts").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

            match stype {
                "open" => counts.open += 1,
                "assigned" => counts.assigned += 1,
                "in_progress" => counts.in_progress += 1,
                "merged" => counts.merged += 1,
                "completed" => counts.completed += 1,
                "failed" => {
                    counts.failed += 1;
                    let reason = status_val
                        .and_then(|v| v.get("reason"))
                        .and_then(|r| r.as_str())
                        .unwrap_or("Unknown failure")
                        .to_string();
                    escalated_tickets.push(EscalatedTicketSummary {
                        id: ticket_id,
                        title,
                        reason,
                        attempts,
                        escalation_type: "failed".to_string(),
                    });
                }
                "exhausted" => {
                    counts.exhausted += 1;
                    escalated_tickets.push(EscalatedTicketSummary {
                        id: ticket_id,
                        title,
                        reason: "Attempts exhausted".to_string(),
                        attempts,
                        escalation_type: "failed".to_string(),
                    });
                }
                "awaiting_human" => {
                    counts.awaiting_human += 1;
                    let reason = status_val
                        .and_then(|v| v.get("reason"))
                        .and_then(|r| r.as_str())
                        .unwrap_or("Human intervention requested")
                        .to_string();
                    escalated_tickets.push(EscalatedTicketSummary {
                        id: ticket_id,
                        title,
                        reason,
                        attempts,
                        escalation_type: "awaiting_human".to_string(),
                    });
                }
                _ => counts.open += 1,
            }
        }

        let worker_slots: HashMap<String, WorkerSlot> =
            t_store.get_typed("worker_slots").await.unwrap_or_default();

        let pending_prs: Vec<serde_json::Value> =
            t_store.get_typed("pending_prs").await.unwrap_or_default();

        let ci_readiness: Option<String> = t_store.get("ci_readiness").await.and_then(|v| {
            if let Some(s) = v.as_str() {
                Some(s.to_string())
            } else {
                v.get("type")
                    .and_then(|t| t.as_str())
                    .map(ToString::to_string)
            }
        });

        // Read fine-grained phases: ticket:{id}:status
        let mut phases = HashMap::new();
        for t in &raw_tickets {
            if let Some(id) = t.get("id").and_then(|v| v.as_str()) {
                if let Some(raw_val) = t_store.get(&format!("ticket:{id}:status")).await {
                    if let Some(obj) = raw_val.as_object() {
                        let phase = obj
                            .get("phase")
                            .and_then(|p| p.as_str())
                            .unwrap_or("unknown")
                            .to_string();
                        let role = obj
                            .get("role")
                            .and_then(|r| r.as_str())
                            .map(ToString::to_string);
                        let ts = obj.get("ts").and_then(|t| t.as_u64());
                        phases.insert(id.to_string(), PhaseStatus { phase, role, ts });
                    }
                }
            }
        }

        // Read heartbeats: heartbeat:{role}-T-{ticket_id}
        let hb_prefix = format!("ns:{}:heartbeat:", tenant);
        let hb_keys = self.store.raw_keys(&format!("{}*", hb_prefix)).await;
        let mut heartbeats = HashMap::new();
        for key in hb_keys {
            if let Some(hb_id) = key.strip_prefix(&hb_prefix) {
                if let Some(record) = self
                    .store
                    .for_tenant(tenant)
                    .get_typed::<HeartbeatRecord>(&format!("heartbeat:{}", hb_id))
                    .await
                {
                    heartbeats.insert(hb_id.to_string(), record);
                }
            }
        }

        let awaiting_human_count = counts.awaiting_human;
        let failed_count = counts.failed + counts.exhausted;

        Ok(TenantFleetResponse {
            tenant: tenant.to_string(),
            repository,
            ticket_counts: counts,
            tickets: raw_tickets,
            phases,
            worker_slots,
            pending_prs,
            ci_readiness,
            heartbeats,
            escalations: EscalationSummary {
                awaiting_human_count,
                failed_count,
                tickets: escalated_tickets,
            },
        })
    }
}
