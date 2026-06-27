use config::state::WorkspaceProvider;
use config::{WorkerSlot, WorkerStatus};

#[derive(Debug, Clone)]
pub struct WorkerInfo {
    pub id: String,
    pub status: String,
    pub detail: String,
    pub workspace_name: Option<String>,
    pub workspace_url: Option<String>,
}

impl WorkerInfo {
    pub fn from_slot(slot: &WorkerSlot, coder_url: Option<&str>) -> Self {
        let (status, detail) = match &slot.status {
            WorkerStatus::Idle => ("IDLE".to_string(), "Waiting for assignment".to_string()),
            WorkerStatus::Assigned { ticket_id, .. } => {
                ("Assigned".to_string(), format!("Ticket {}", ticket_id))
            }
            WorkerStatus::Working { ticket_id, .. } => {
                ("WORKING".to_string(), format!("Ticket {}", ticket_id))
            }
            WorkerStatus::Done { ticket_id, outcome } => (
                "Done".to_string(),
                format!("Ticket {} ({})", ticket_id, outcome),
            ),
            WorkerStatus::Suspended {
                ticket_id, reason, ..
            } => (
                "Suspended".to_string(),
                format!("Ticket {} ({})", ticket_id, reason),
            ),
        };

        let (workspace_name, workspace_url) = match (
            &slot.workspace_provider,
            slot.workspace_id.as_deref(),
            coder_url,
        ) {
            (WorkspaceProvider::Coder, Some(workspace_id), Some(base_url)) => {
                let base = base_url.trim_end_matches('/');
                (
                    Some(workspace_id.to_string()),
                    Some(format!("{}/workspaces/{}", base, workspace_id)),
                )
            }
            _ => (None, None),
        };

        Self {
            id: slot.id.clone(),
            status,
            detail,
            workspace_name,
            workspace_url,
        }
    }
}
