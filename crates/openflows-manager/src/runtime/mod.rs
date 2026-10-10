//! WP-04 runtime authentication and the GitHub credential broker.
//!
//! This module introduces a runtime principal distinct from human browser/CLI
//! sessions and human API credentials. A workspace runtime credential is a
//! hash-stored bearer secret bound to a workspace; its organization and tenant
//! are derived from trusted database relationships (workspace -> tenant ->
//! organization -> connection -> repository). Issuance, rotation, and
//! revocation are internal services; there is deliberately no unauthenticated
//! bootstrap endpoint and no shared global runtime password.
//!
//! The credential broker exchanges a short-lived GitHub installation token
//! restricted to the authenticated workspace's authorized repository and a
//! server-controlled permission profile, and records an encrypted credential
//! lease for revocation and cleanup.

pub mod broker;
pub mod credentials;
pub mod permissions;
pub mod repository;

pub use crate::id::CredentialLeaseId;
pub use broker::{BrokerCredential, CredentialBroker, INSTALLATION_TOKEN_USERNAME};
pub use credentials::{RuntimeCredentialIssue, RuntimeCredentialService};
pub use permissions::{resolve_profile, PermissionProfile, RuntimePurpose};
pub use repository::{
    CredentialLease, NewCredentialLease, RuntimeRepository, RuntimeScope, WORKSPACE_AUDIENCE,
};
