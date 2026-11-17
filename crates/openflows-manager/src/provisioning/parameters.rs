//! Rich parameter values a hosted workspace is created with.
//!
//! The templates declare these parameters; the Manager fills them per workspace.
//! Keeping the lists here lets `tenant_worker` stay within its line budget and
//! puts the backend-specific placement in one place.

use crate::id::{OrganizationId, TenantId};
use crate::templates::ProvisionerBackend;
use coder_client::types::WorkspaceParameterValue;

/// What a hosted Nexus workspace is created with.
pub struct NexusParameters<'a> {
    pub hosted: bool,
    pub runtime_secret_ref: &'a str,
    pub workspace_image_digest: &'a str,
    pub tenant_id: TenantId,
    pub repository: &'a str,
    pub fleet_size: i32,
    pub organization_id: OrganizationId,
    pub backend: ProvisionerBackend,
}

/// Parameters for the tenant's Nexus workspace.
pub fn nexus_parameters(params: NexusParameters<'_>) -> Vec<WorkspaceParameterValue> {
    let NexusParameters {
        hosted,
        runtime_secret_ref,
        workspace_image_digest,
        tenant_id,
        repository,
        fleet_size,
        organization_id,
        backend,
    } = params;
    let mut parameters = vec![
        value("hosted", hosted.to_string()),
        value("runtime_secret_ref", runtime_secret_ref),
        value("workspace_image_digest", workspace_image_digest),
        value("tenant", tenant_id.to_string()),
        value("repo_url", format!("https://github.com/{repository}.git")),
        value("github_repository", repository),
        value("fleet_size", fleet_size.to_string()),
    ];
    parameters.extend(kubernetes_placement_parameters(backend, organization_id));
    parameters
}

/// Parameters that place a hosted workspace inside its organization's Kubernetes
/// namespace. The Manager delivers the runtime Secret there, so the pod has to
/// land beside it; the Docker backend has no namespaces and gets none.
pub fn kubernetes_placement_parameters(
    backend: ProvisionerBackend,
    organization_id: OrganizationId,
) -> Vec<WorkspaceParameterValue> {
    if backend != ProvisionerBackend::Kubernetes {
        return Vec::new();
    }
    vec![
        value("organization", organization_id.to_string()),
        value(
            "runtime_namespace",
            crate::runtime::delivery::organization_namespace(organization_id),
        ),
    ]
}

fn value(name: &str, value: impl Into<String>) -> WorkspaceParameterValue {
    WorkspaceParameterValue {
        name: name.to_string(),
        value: value.into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn docker_workspaces_carry_no_kubernetes_placement() {
        let parameters = kubernetes_placement_parameters(ProvisionerBackend::Docker, org());
        assert!(parameters.is_empty());
    }

    #[test]
    fn kubernetes_workspaces_land_in_their_organization_namespace() {
        let parameters = kubernetes_placement_parameters(ProvisionerBackend::Kubernetes, org());
        let names: Vec<&str> = parameters.iter().map(|p| p.name.as_str()).collect();
        assert_eq!(names, ["organization", "runtime_namespace"]);
        assert_eq!(parameters[0].value, org().to_string());
        assert_eq!(parameters[1].value, format!("openflows-{}", org()));
    }

    #[test]
    fn nexus_carries_its_repository_and_digest() {
        let parameters = nexus_parameters(NexusParameters {
            hosted: true,
            runtime_secret_ref: "openflows-runtime-1",
            workspace_image_digest: "sha256:abc",
            tenant_id: TenantId::new(),
            repository: "acme/widgets",
            fleet_size: 2,
            organization_id: org(),
            backend: ProvisionerBackend::Kubernetes,
        });
        let get = |name: &str| {
            parameters
                .iter()
                .find(|p| p.name == name)
                .unwrap_or_else(|| panic!("missing {name}"))
                .value
                .clone()
        };
        assert_eq!(get("hosted"), "true");
        assert_eq!(get("repo_url"), "https://github.com/acme/widgets.git");
        assert_eq!(get("github_repository"), "acme/widgets");
        assert_eq!(get("fleet_size"), "2");
        assert!(get("runtime_namespace").starts_with("openflows-"));
    }

    fn org() -> OrganizationId {
        OrganizationId::from_uuid(uuid::Uuid::from_u128(7))
    }
}
