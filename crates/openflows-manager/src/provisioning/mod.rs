//! Durable organization provisioning and readiness reconciliation.
//!
//! [`OrgProvisionWorker`] claims `org.provision` operations and executes the
//! readiness steps (capabilities, Coder organization, provisioner, template
//! publication, model/chat capability) before marking an organization ready.
//! It is durable and recoverable: progress is persisted, stale lease holders
//! are fenced, and a suspended/deleted organization is never resurrected to
//! ready. The operator-configured release artifacts come from a
//! [`ReleaseSource`] (local filesystem or a pinned hosted GitHub release),
//! never from customer-supplied URLs.

pub mod docker;
pub mod local_workspace_cleanup;
pub mod release_provider;
pub mod selection;
pub mod source;
pub mod tenant_worker;
pub mod worker;
pub mod workspace_lifecycle;

pub use release_provider::{ReleaseProvider, ReleaseProviderError};
pub use selection::{
    compatibility_target_from_env, record_workspace_and_tenant_pin,
    resolve_nexus_release_for_tenant, resolve_worker_release_for_tenant,
    validate_artifacts_for_target, WorkspaceReleaseSpec,
};
pub use source::{ReleaseCache, ReleaseSource, ResolvedRelease};
pub use tenant_worker::{err as tenant_err, TenantProvisionOutcome, TenantProvisionWorker};
pub use worker::{err, OrgProvisionOutcome, OrgProvisionWorker};
