//! Types for the Coder adapter.

/// The result of verifying a Coder deployment's capabilities during
/// organization readiness reconciliation. Version and license are prerequisites
/// for provisioning; model/chat capability is independent of template readiness.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CoderCapabilities {
    /// The operator-required Coder version baseline is met.
    pub version_ok: bool,
    /// The deployment has an applicable license (organizations/service
    /// accounts require Premium). Never fabricated.
    pub license_ok: bool,
    /// The deployment exposes organization-scoped Agents/chat model listing.
    pub agents_chat_ok: bool,
}

/// A template version successfully published into an explicit organization,
/// with the exact upstream IDs persisted and re-verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PublishedTemplate {
    pub template_id: coder_client::CoderTemplateId,
    pub version_id: coder_client::CoderTemplateVersionId,
    pub import_job_id: String,
}

/// Whether a publication attempt created new resources or converged on
/// already-published ones. Used to assert retry convergence (no unbounded
/// duplicate versions).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PublishOutcome {
    /// A new template/version was created by this attempt.
    Created,
    /// An existing, verified publication was rediscovered.
    Reconciled,
}

/// The result of ensuring a template version is published in an organization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TemplatePublicationResult {
    pub published: PublishedTemplate,
    pub outcome: PublishOutcome,
    /// Deterministic name the template is stored under in this organization.
    pub template_name: String,
    /// Deterministic version name used for the publication.
    pub version_name: String,
}

/// Re-export the org id type for convenience.
pub use coder_client::CoderOrganizationId as OrgId;

/// The Coder machine owner (service account) id that owns a tenant's
/// workspaces. Verified to belong only to the tenant's organization.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct CoderOwnerId(pub String);

impl CoderOwnerId {
    pub fn new(id: impl Into<String>) -> Self {
        CoderOwnerId(id.into())
    }
}
