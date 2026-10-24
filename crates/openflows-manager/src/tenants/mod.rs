//! Tenant lifecycle service.
//!
//! Implements authorized organization-scoped tenant creation, listing, detail,
//! and deletion. Every tenant is bound to an accessible, immutable repository ID
//! in the caller's organization; the binding is revalidated server-side on every
//! mutation and enforced at the database level by composite foreign keys.
//!
//! Creation enqueues a durable `tenant.provision` operation (202) and reserves
//! tenant capacity against the organization's quota ceiling transactionally, so
//! concurrent requests cannot bypass the minimal infrastructure safeguards.
//! When the organization's provisioning-execution slot is occupied, the
//! operation is queued rather than rejected as a resource-limit violation.
//!
//! Deletion (admin only, 202) records the desired `deleting` state, revokes
//! runtime issuance (worker/redis/hook credentials), and enqueues a durable
//! cleanup operation that reconciles workspace and tenant resource cleanup
//! safely. External effects are never awaited synchronously.

pub mod service;
pub use service::{
    validate_tenant_slug, TenantOperationResponse, TenantService, TenantServiceInputs,
};
