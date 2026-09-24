//! Kanban and normalized ticket service.

use crate::{
    error::ManagerError,
    models::{
        fleet::PhaseStatus,
        kanban::{
            KanbanStage, NormalizedTicket, NormalizedTicketDetail, TicketGateRecord,
            TicketReviewRecord,
        },
    },
    services::tenant::validate_tenant_name,
};
use pocketflow_core::SharedStore;

/// Service for ticket normalization and Kanban stage resolution.
#[derive(Clone)]
pub struct KanbanService {
    store: SharedStore,
}

impl KanbanService {
    pub fn new(store: SharedStore) -> Self {
        Self { store }
    }

    /// Derive the canonical Kanban stage from high-level status and fine-grained phase.
    pub fn derive_stage(
        status_type: &str,
        phase: Option<&PhaseStatus>,
    ) -> (KanbanStage, &'static str) {
        let stage = match status_type {
            "awaiting_human" => KanbanStage::AwaitingHuman,
            "failed" | "exhausted" => KanbanStage::Failed,
            "merged" | "completed" => KanbanStage::Done,
            "open" => {
                if let Some(p) = phase {
                    match p.phase.as_str() {
                        "planning" => KanbanStage::Planning,
                        "building" => KanbanStage::Building,
                        "testing" => KanbanStage::Testing,
                        "review_ready" => KanbanStage::Review,
                        "blocked" => KanbanStage::AwaitingHuman,
                        _ => KanbanStage::Open,
                    }
                } else {
                    KanbanStage::Open
                }
            }
            "assigned" | "in_progress" => {
                if let Some(p) = phase {
                    match p.phase.as_str() {
                        "planning" => KanbanStage::Planning,
                        "building" => KanbanStage::Building,
                        "testing" => KanbanStage::Testing,
                        "review_ready" => KanbanStage::Review,
                        "blocked" => KanbanStage::AwaitingHuman,
                        _ => {
                            if status_type == "assigned" {
                                KanbanStage::Planning
                            } else {
                                KanbanStage::Building
                            }
                        }
                    }
                } else if status_type == "assigned" {
                    KanbanStage::Planning
                } else {
                    KanbanStage::Building
                }
            }
            _ => KanbanStage::Open,
        };

        (stage, stage.label())
    }

    /// List normalized tickets for a given tenant.
    pub async fn list_normalized_tickets(
        &self,
        tenant: &str,
    ) -> Result<Vec<NormalizedTicket>, ManagerError> {
        validate_tenant_name(tenant)?;

        let pattern = format!("ns:{}:*", tenant);
        let keys = self.store.raw_keys(&pattern).await;
        if keys.is_empty() {
            return Err(ManagerError::TenantNotFound(tenant.to_string()));
        }

        let t_store = self.store.for_tenant(tenant);
        let raw_tickets: Vec<serde_json::Value> =
            t_store.get_typed("tickets").await.unwrap_or_default();

        let mut normalized = Vec::with_capacity(raw_tickets.len());

        for raw in raw_tickets {
            let id = raw
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let title = raw
                .get("title")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let body = raw
                .get("body")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
            let priority = raw.get("priority").and_then(|v| v.as_u64()).unwrap_or(0) as u32;
            let branch = raw
                .get("branch")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
            let issue_url = raw
                .get("issue_url")
                .and_then(|v| v.as_str())
                .map(ToString::to_string);
            let attempts = raw.get("attempts").and_then(|v| v.as_u64()).unwrap_or(0) as u32;

            let raw_status = raw
                .get("status")
                .cloned()
                .unwrap_or(serde_json::json!({"type": "open"}));
            let status_type = match &raw_status {
                serde_json::Value::Object(obj) => {
                    obj.get("type").and_then(|v| v.as_str()).unwrap_or("open")
                }
                serde_json::Value::String(s) => s.as_str(),
                _ => "open",
            }
            .to_string();

            let assigned_worker = match &raw_status {
                serde_json::Value::Object(obj) => obj
                    .get("worker_id")
                    .and_then(|v| v.as_str())
                    .map(ToString::to_string),
                _ => None,
            };

            // Read fine-grained phase if present
            let phase: Option<PhaseStatus> = t_store
                .get(&format!("ticket:{}:status", id))
                .await
                .and_then(|v| {
                    let obj = v.as_object()?;
                    let phase = obj.get("phase")?.as_str()?.to_string();
                    let role = obj
                        .get("role")
                        .and_then(|r| r.as_str())
                        .map(ToString::to_string);
                    let ts = obj.get("ts").and_then(|t| t.as_u64());
                    Some(PhaseStatus { phase, role, ts })
                });

            let (stage, stage_label) = Self::derive_stage(&status_type, phase.as_ref());

            normalized.push(NormalizedTicket {
                id,
                title,
                body,
                priority,
                branch,
                issue_url,
                attempts,
                raw_status,
                status_type: status_type.to_string(),
                stage,
                stage_label: stage_label.to_string(),
                assigned_worker,
                phase,
            });
        }

        Ok(normalized)
    }

    /// Retrieve full normalized ticket detail including PR, reviews, gates, handoff, and deployment.
    pub async fn get_normalized_ticket_detail(
        &self,
        tenant: &str,
        ticket_id: &str,
    ) -> Result<NormalizedTicketDetail, ManagerError> {
        validate_tenant_name(tenant)?;

        if ticket_id.trim().is_empty() {
            return Err(ManagerError::InvalidRequest(
                "ticket ID must not be empty".to_string(),
            ));
        }

        let pattern = format!("ns:{}:*", tenant);
        let keys = self.store.raw_keys(&pattern).await;
        if keys.is_empty() {
            return Err(ManagerError::TenantNotFound(tenant.to_string()));
        }

        let t_store = self.store.for_tenant(tenant);
        let raw_tickets: Vec<serde_json::Value> =
            t_store.get_typed("tickets").await.unwrap_or_default();

        let raw_ticket = raw_tickets
            .into_iter()
            .find(|t| t.get("id").and_then(|v| v.as_str()) == Some(ticket_id))
            .ok_or_else(|| ManagerError::TicketNotFound {
                tenant: tenant.to_string(),
                ticket: ticket_id.to_string(),
            })?;

        let id = ticket_id.to_string();
        let title = raw_ticket
            .get("title")
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let body = raw_ticket
            .get("body")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let priority = raw_ticket
            .get("priority")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;
        let branch = raw_ticket
            .get("branch")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let issue_url = raw_ticket
            .get("issue_url")
            .and_then(|v| v.as_str())
            .map(ToString::to_string);
        let attempts = raw_ticket
            .get("attempts")
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as u32;

        let raw_status = raw_ticket
            .get("status")
            .cloned()
            .unwrap_or(serde_json::json!({"type": "open"}));
        let status_type = match &raw_status {
            serde_json::Value::Object(obj) => {
                obj.get("type").and_then(|v| v.as_str()).unwrap_or("open")
            }
            serde_json::Value::String(s) => s.as_str(),
            _ => "open",
        }
        .to_string();

        let assigned_worker = match &raw_status {
            serde_json::Value::Object(obj) => obj
                .get("worker_id")
                .and_then(|v| v.as_str())
                .map(ToString::to_string),
            _ => None,
        };

        // Fine-grained phase
        let phase: Option<PhaseStatus> = t_store
            .get(&format!("ticket:{}:status", id))
            .await
            .and_then(|v| {
                let obj = v.as_object()?;
                let phase = obj.get("phase")?.as_str()?.to_string();
                let role = obj
                    .get("role")
                    .and_then(|r| r.as_str())
                    .map(ToString::to_string);
                let ts = obj.get("ts").and_then(|t| t.as_u64());
                Some(PhaseStatus { phase, role, ts })
            });

        let (stage, stage_label) = Self::derive_stage(&status_type, phase.as_ref());

        // Artifacts
        let pr = t_store.get(&format!("ticket:{}:pr", id)).await;
        let handoff = t_store.get(&format!("ticket:{}:handoff", id)).await;
        let deployment = t_store.get(&format!("ticket:{}:deployment", id)).await;

        // Pending PR
        let pending_prs: Vec<serde_json::Value> =
            t_store.get_typed("pending_prs").await.unwrap_or_default();
        let pending_pr = pending_prs.into_iter().find(|p| {
            p.get("ticket_id")
                .and_then(|v| v.as_str())
                .map(|t| t == ticket_id)
                .unwrap_or(false)
        });

        // Reviews (sentinel default + any review keys)
        let mut reviews = Vec::new();
        let review_prefix = format!("ns:{}:ticket:{}:review:", tenant, id);
        let review_keys = self.store.raw_keys(&format!("{}*", review_prefix)).await;
        for key in review_keys {
            if let Some(role) = key.strip_prefix(&review_prefix) {
                if let Some(payload) = t_store.get(&format!("ticket:{}:review:{}", id, role)).await
                {
                    reviews.push(TicketReviewRecord {
                        role: role.to_string(),
                        payload,
                    });
                }
            }
        }
        let review = t_store.get(&format!("ticket:{}:review:sentinel", id)).await;

        // Gates
        let mut gates = Vec::new();
        let gate_prefix = format!("ns:{}:ticket:{}:gate:", tenant, id);
        let gate_keys = self.store.raw_keys(&format!("{}*", gate_prefix)).await;
        for key in gate_keys {
            if let Some(gphase) = key.strip_prefix(&gate_prefix) {
                if let Some(payload) = t_store.get(&format!("ticket:{}:gate:{}", id, gphase)).await
                {
                    gates.push(TicketGateRecord {
                        phase: gphase.to_string(),
                        payload,
                    });
                }
            }
        }
        let gate = t_store.get(&format!("ticket:{}:gate:planning", id)).await;

        Ok(NormalizedTicketDetail {
            ticket: NormalizedTicket {
                id,
                title,
                body,
                priority,
                branch,
                issue_url,
                attempts,
                raw_status,
                status_type: status_type.to_string(),
                stage,
                stage_label: stage_label.to_string(),
                assigned_worker,
                phase,
            },
            tenant: tenant.to_string(),
            pr,
            pending_pr,
            review,
            reviews,
            gate,
            gates,
            handoff,
            deployment,
        })
    }
}
