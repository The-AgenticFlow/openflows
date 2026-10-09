//! Organization-scoped repositories.
//!
//! Every repository method takes an explicit [`OrgScope`] (or the scope lives in
//! the transaction type) and joins every lookup to `organization_id`. A bare
//! resource id is never sufficient to read or modify a record, so organization
//! A cannot reach organization B's data through these interfaces.

pub mod organizations;
pub mod tenants;

pub use organizations::OrganizationsRepository;
pub use tenants::TenantsRepository;
