//! Service layer implementations for OpenFlows Manager.

pub mod ai;
pub mod fleet;
pub mod kanban;
pub mod tenant;

pub use ai::{AiBackend, AiService, CoderAiBackend, MockAiBackend};
pub use fleet::FleetService;
pub use kanban::KanbanService;
pub use tenant::{validate_repository, validate_tenant_name, TenantProvisioner, TenantService};
