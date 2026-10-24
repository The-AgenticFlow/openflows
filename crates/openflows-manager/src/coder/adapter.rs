//! The Coder adapter trait and real implementation.

use crate::coder::types::{
    CoderCapabilities, PublishOutcome, PublishedTemplate, TemplatePublicationResult,
};
use crate::error::ManagerError;
use async_trait::async_trait;
use coder_client::{
    CoderClient, CoderOrganizationId, CreateOrganizationRequest, CreateTemplateRequest,
};
use std::time::Duration;

/// A testable Coder adapter.
///
/// All operations are explicitly organization-scoped and verify the returned
/// resource's scope. The manager's provisioning worker and release service
/// depend only on this trait, so the offline tests can drive a deterministic
/// in-memory implementation while the real adapter talks to a pinned Coder
/// deployment.
#[async_trait]
pub trait CoderAdapter: Send + Sync {
    /// Recover already-managed deployment infrastructure after restarts.
    async fn reconcile_provisioners(&self) -> Result<(), ManagerError> {
        Ok(())
    }
    /// Verify the deployment's version, license, and org-scoped Agents/chat
    /// capabilities. Returns explicit capability booleans; the caller turns
    /// missing prerequisites into a not-ready state.
    async fn verify_capabilities(&self) -> Result<CoderCapabilities, ManagerError>;

    /// Ensure a dedicated Coder organization with `internal_name` exists and
    /// returns its verified id. Creates it if absent. Never resolves the
    /// default organization.
    async fn ensure_organization(
        &self,
        internal_name: &str,
        display_name: &str,
    ) -> Result<CoderOrganizationId, ManagerError>;

    /// Ensure an organization-scoped provisioner is ready (heartbeat verified).
    /// `backend` names the configured backend adapter. Returns an error that
    /// the worker maps to an explicit not-ready state when the provisioner is
    /// unavailable.
    async fn ensure_provisioner_ready(
        &self,
        org: &CoderOrganizationId,
        backend: &str,
    ) -> Result<(), ManagerError>;

    /// Ensure `version_name` of the deterministic `template_name` template is
    /// published into `org` and its import job succeeded. Idempotent: on retry
    /// it rediscovers and verifies the existing publication rather than
    /// appending unbounded duplicate versions. Returns the exact persisted IDs.
    async fn ensure_template_version(
        &self,
        org: &CoderOrganizationId,
        template_name: &str,
        version_name: &str,
        archive: &[u8],
    ) -> Result<TemplatePublicationResult, ManagerError>;

    /// Verify org-scoped model availability and Agents/chat capability. Returns
    /// true when at least one org-scoped model is reachable.
    async fn verify_org_models(&self, org: &CoderOrganizationId) -> Result<bool, ManagerError>;

    // ------------------------------------------------------------------
    // Tenant runtime provisioning.
    //
    // These operations create and verify tenant runtime resources in a Coder
    // organization with explicit organization, machine-owner, template, and
    // pinned-version IDs. They never fall back to the default organization or
    // the current operator user. Implementations must verify the returned
    // resource's ownership before adopting it.
    //
    // The default implementations fail closed with an explicit
    // TENANT_RUNTIME_UNSUPPORTED error so a deployment that has not wired a
    // tenant-capable backend cannot silently degrade. A licensed Coder
    // deployment is required to exercise the real implementations.
    // ------------------------------------------------------------------

    /// Ensure the tenant's machine identity (Coder owner) exists in `org` and
    /// belongs only to `org`. Returns the verified owner id. The owner is the
    /// tenant machine identity that owns workspaces, not a human admin.
    async fn ensure_machine_owner(
        &self,
        _org: &CoderOrganizationId,
        _tenant_id: crate::id::TenantId,
    ) -> Result<crate::coder::types::CoderOwnerId, ManagerError> {
        Err(ManagerError::api(
            "TENANT_RUNTIME_UNSUPPORTED",
            "tenant machine-owner provisioning is not configured",
        ))
    }

    /// Create (or rediscover-and-verify) a tenant workspace with explicit
    /// organization, owner, template, and pinned version. Returns the verified
    /// workspace detail; the caller persists the exact IDs.
    async fn ensure_user_owner(
        &self,
        _org: &CoderOrganizationId,
        _owner: &crate::coder::types::CoderOwnerId,
    ) -> Result<(), ManagerError> {
        Err(ManagerError::api(
            "CODER_OWNER_UNSUPPORTED",
            "personal Coder ownership is not supported by this adapter",
        ))
    }

    async fn create_tenant_workspace(
        &self,
        _org: &CoderOrganizationId,
        _owner: &crate::coder::types::CoderOwnerId,
        _template_id: &coder_client::CoderTemplateId,
        _version_id: &coder_client::CoderTemplateVersionId,
        _name: &str,
    ) -> Result<coder_client::CoderWorkspaceDetail, ManagerError> {
        Err(ManagerError::api(
            "TENANT_RUNTIME_UNSUPPORTED",
            "tenant workspace provisioning is not configured",
        ))
    }

    /// Create a tenant workspace while passing only validated, non-secret
    /// references/metadata to the template. Secret values are resolved and
    /// mounted by the provisioner backend, never embedded in Terraform state.
    async fn create_tenant_workspace_with_parameters(
        &self,
        org: &CoderOrganizationId,
        owner: &crate::coder::types::CoderOwnerId,
        template_id: &coder_client::CoderTemplateId,
        version_id: &coder_client::CoderTemplateVersionId,
        name: &str,
        _parameters: &[coder_client::types::WorkspaceParameterValue],
    ) -> Result<coder_client::CoderWorkspaceDetail, ManagerError> {
        self.create_tenant_workspace(org, owner, template_id, version_id, name)
            .await
    }

    /// Wait for a tenant workspace's build to succeed and its agent to connect,
    /// bounded. Returns an error (mapped to a retryable step) on timeout.
    async fn wait_workspace_ready(
        &self,
        _org: &CoderOrganizationId,
        _workspace_id: &str,
    ) -> Result<(), ManagerError> {
        Err(ManagerError::api(
            "TENANT_RUNTIME_UNSUPPORTED",
            "tenant workspace readiness is not configured",
        ))
    }

    async fn delete_workspace(
        &self,
        _org: &CoderOrganizationId,
        _workspace_id: &str,
    ) -> Result<(), ManagerError> {
        Err(ManagerError::api(
            "TENANT_RUNTIME_UNSUPPORTED",
            "tenant workspace deletion is not configured",
        ))
    }

    async fn start_workspace(
        &self,
        _org: &CoderOrganizationId,
        _workspace_id: &str,
    ) -> Result<(), ManagerError> {
        Err(ManagerError::api(
            "TENANT_RUNTIME_UNSUPPORTED",
            "tenant workspace start is not configured",
        ))
    }

    /// Create a fresh Coder start build for a workspace that may already be
    /// running. This is required after the manager rotates runtime bootstrap
    /// input following a failed startup attempt.
    async fn restart_workspace(
        &self,
        _org: &CoderOrganizationId,
        _workspace_id: &str,
    ) -> Result<(), ManagerError> {
        Err(ManagerError::api(
            "TENANT_RUNTIME_UNSUPPORTED",
            "tenant workspace restart is not configured",
        ))
    }

    async fn stop_workspace(
        &self,
        _org: &CoderOrganizationId,
        _workspace_id: &str,
    ) -> Result<(), ManagerError> {
        Err(ManagerError::api(
            "TENANT_RUNTIME_UNSUPPORTED",
            "tenant workspace stop is not configured",
        ))
    }

    /// Delete the tenant's dedicated machine owner after all workspaces have
    /// been removed. Implementations must never delete a human or shared user.
    async fn delete_machine_owner(
        &self,
        _org: &CoderOrganizationId,
        _owner: &crate::coder::types::CoderOwnerId,
    ) -> Result<(), ManagerError> {
        Err(ManagerError::api(
            "TENANT_RUNTIME_UNSUPPORTED",
            "tenant machine-owner deletion is not configured",
        ))
    }
}

/// Polling and lease defaults for the real adapter.
const IMPORT_POLL_INTERVAL: Duration = Duration::from_secs(2);
const IMPORT_POLL_TIMEOUT: Duration = Duration::from_secs(120);
// First-time imports download Terraform providers inside the provisioner.
// GitHub release assets can be slow in local Docker networking, so a 2-minute
// deadline repeatedly abandoned otherwise-active import jobs.
const TEMPLATE_IMPORT_TIMEOUT: Duration = Duration::from_secs(600);

/// A real adapter backed by `coder-client` and an operator-scoped Coder token.
#[derive(Clone)]
pub struct RealCoderAdapter {
    client: CoderClient,
    docker_provisioner: Option<crate::provisioning::docker::DockerProvisioner>,
    /// The pinned version baseline, e.g. "v2.37.3".
    version_baseline: String,
}

fn retryable_import_error(error: ManagerError, attempt: usize) -> ManagerError {
    if attempt < 2 && matches!(&error, ManagerError::Api(api) if api.code == "IMPORT_FAILED") {
        ManagerError::api(
            "IMPORT_RETRYABLE",
            "template import failed; a bounded replacement import will be attempted",
        )
    } else {
        error
    }
}

impl RealCoderAdapter {
    pub fn with_shared_operator_token(
        mut self,
        token: std::sync::Arc<std::sync::RwLock<String>>,
    ) -> Self {
        self.client = self.client.with_shared_token(token);
        self
    }
    pub fn new(client: CoderClient, version_baseline: impl Into<String>) -> Self {
        RealCoderAdapter {
            client,
            docker_provisioner: None,
            version_baseline: version_baseline.into(),
        }
    }

    pub fn with_docker_provisioner(
        mut self,
        provisioner: crate::provisioning::docker::DockerProvisioner,
    ) -> Self {
        self.docker_provisioner = Some(provisioner);
        self
    }

    /// Build the adapter from the manager's Coder URL and operator token. The
    /// operator token is resolved by the caller from the secret provider and
    /// never logged or persisted by this adapter.
    pub fn from_parts(
        coder_url: &str,
        operator_token: &str,
        version_baseline: impl Into<String>,
    ) -> Self {
        RealCoderAdapter::new(
            CoderClient::new(coder_url, operator_token),
            version_baseline,
        )
    }
}

#[async_trait]
impl CoderAdapter for RealCoderAdapter {
    async fn reconcile_provisioners(&self) -> Result<(), ManagerError> {
        if let Some(provisioner) = &self.docker_provisioner {
            provisioner.reconcile(&self.client).await?;
        }
        Ok(())
    }
    async fn verify_capabilities(&self) -> Result<CoderCapabilities, ManagerError> {
        // Version: `GET /api/v2/buildinfo` is unauthenticated. We compare the
        // reported version prefix against the pinned baseline. Because live
        // deployment access is an operator gate, a failure here maps to
        // an explicit not-ready state, never a privileged fallback.
        let buildinfo: Result<serde_json::Value, _> = {
            let resp = self
                .client
                .raw_get("/api/v2/buildinfo")
                .await
                .map_err(ManagerError::from);
            resp
        };
        let version_ok = match buildinfo {
            Ok(v) => v
                .get("version")
                .and_then(|x| x.as_str())
                .map(|v| v.starts_with(&self.version_baseline) || v == self.version_baseline)
                .unwrap_or(false),
            Err(_) => false,
        };

        // License: organizations/service accounts require Premium. We cannot
        // fabricate this; probe the org-scoped chat-models endpoint, which
        // requires org membership and Agents access, as a conservative license
        // + capability signal. A failure is a not-ready prerequisite.
        let default_org_probe = self.probe_license().await;

        Ok(CoderCapabilities {
            version_ok,
            license_ok: default_org_probe,
            agents_chat_ok: default_org_probe,
        })
    }

    async fn ensure_organization(
        &self,
        internal_name: &str,
        display_name: &str,
    ) -> Result<CoderOrganizationId, ManagerError> {
        let orgs = self
            .client
            .list_organizations()
            .await
            .map_err(ManagerError::from)?;
        // Deterministic internal name: if it exists, verify its scope.
        if let Some(existing) = orgs.iter().find(|o| o.name == internal_name) {
            if existing.id.is_empty() {
                return Err(ManagerError::Service(anyhow::anyhow!(
                    "Coder organization '{}' reported no id",
                    internal_name
                )));
            }
            return Ok(CoderOrganizationId::new(existing.id.clone()));
        }
        // Create it and verify the returned id.
        let created = self
            .client
            .create_organization(&CreateOrganizationRequest {
                name: internal_name.to_string(),
                display_name: Some(display_name.to_string()),
                description: Some("OpenFlows-managed organization".to_string()),
            })
            .await
            .map_err(ManagerError::from)?;
        if created.id.is_empty() {
            return Err(ManagerError::Service(anyhow::anyhow!(
                "Coder organization creation returned no id"
            )));
        }
        Ok(CoderOrganizationId::new(created.id))
    }

    async fn ensure_provisioner_ready(
        &self,
        org: &CoderOrganizationId,
        backend: &str,
    ) -> Result<(), ManagerError> {
        // Docker reserves and persists one key and container per organization.
        // Other deployment backends retain operator-managed readiness.
        let key_name = if backend == "docker" {
            match &self.docker_provisioner {
                Some(provisioner) => provisioner.ensure(&self.client, org).await?,
                None => format!("openflows-provisioner-{}", org.0),
            }
        } else {
            format!("openflows-provisioner-{}", org.0)
        };

        // Container startup alone is insufficient: require Coder to report an
        // online daemon authenticated using this organization's exact key.
        let deadline = tokio::time::Instant::now() + IMPORT_POLL_TIMEOUT;
        loop {
            let daemons = self
                .client
                .list_provisioner_daemons(org)
                .await
                .map_err(ManagerError::from)?;
            if daemons.iter().any(|d| daemon_is_online(d, &key_name)) {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ManagerError::api(
                    "PROVISIONER_NOT_READY",
                    format!(
                        "organization provisioner for backend '{backend}' is not ready within timeout"
                    ),
                ));
            }
            tokio::time::sleep(IMPORT_POLL_INTERVAL).await;
        }
    }

    async fn ensure_template_version(
        &self,
        org: &CoderOrganizationId,
        template_name: &str,
        version_name: &str,
        archive: &[u8],
    ) -> Result<TemplatePublicationResult, ManagerError> {
        let variables = coder_client::template_variable_values()
            .into_iter()
            .map(|(name, value)| coder_client::types::TemplateVariableValue {
                name: name.to_owned(),
                value,
            })
            .collect::<Vec<_>>();
        // 1. Find or create the template by its deterministic name within the
        //    explicit org.
        let templates = self
            .client
            .list_templates_in_org(org)
            .await
            .map_err(ManagerError::from)?;
        let template = templates.into_iter().find(|t| t.name == template_name);

        let template_id = match template {
            Some(t) => {
                if !t.organization_id.is_empty() && t.organization_id != org.0 {
                    return Err(ManagerError::api(
                        "CODER_SCOPE_MISMATCH",
                        "existing template belongs to a different Coder organization",
                    ));
                }
                coder_client::CoderTemplateId::new(t.id)
            }
            None => {
                // Coder creates the first version independently, then binds it
                // while creating the template.
                let version = self
                    .client
                    .create_template_version_with_variables(
                        org,
                        None,
                        version_name,
                        archive,
                        &variables,
                    )
                    .await
                    .map_err(ManagerError::from)?;
                let created = self
                    .client
                    .create_template(
                        org,
                        &CreateTemplateRequest {
                            name: template_name.to_string(),
                            description: Some("OpenFlows-managed role template".to_string()),
                            default_ttl_ms: None,
                            active_version_id: Some(version.id.clone()),
                        },
                    )
                    .await
                    .map_err(ManagerError::from)?;
                // The version id recorded above belongs to the newly created
                // template; return it.
                let published = PublishedTemplate {
                    template_id: coder_client::CoderTemplateId::new(created.id),
                    version_id: coder_client::CoderTemplateVersionId::new(version.id),
                    import_job_id: version
                        .job
                        .as_ref()
                        .map(|j| j.id.clone())
                        .unwrap_or_default(),
                };
                // If the version has no import job yet, poll it.
                if published.import_job_id.is_empty() {
                    return Err(ManagerError::api(
                        "IMPORT_JOB_UNKNOWN",
                        "created template version did not report an import job",
                    ));
                }
                self.await_import(org, &published.version_id.0)
                    .await
                    .map_err(|e| retryable_import_error(e, 0))?;
                return Ok(TemplatePublicationResult {
                    published,
                    outcome: PublishOutcome::Created,
                    template_name: template_name.to_string(),
                    version_name: version_name.to_string(),
                });
            }
        };

        // 2. Template exists. Look for a version with our deterministic name.
        let versions = self
            .client
            .list_template_versions(org, &template_id)
            .await
            .map_err(ManagerError::from)?;
        // Failed Terraform jobs cannot be resumed. Use bounded, deterministic
        // replacement names so retries converge without accumulating versions.
        // Coder's job error does not reliably distinguish network failures from
        // invalid Terraform, so both receive at most three import attempts.
        let mut attempt = 0;
        let mut import_name = version_name.to_string();
        loop {
            let failed = versions
                .iter()
                .find(|v| v.name == import_name)
                .and_then(|v| v.job.as_ref())
                .is_some_and(|job| job.status == "failed");
            if !failed {
                break;
            }
            if attempt == 2 {
                return Err(ManagerError::api(
                    "IMPORT_FAILED",
                    "template import failed after three attempts",
                ));
            }
            attempt += 1;
            import_name = format!("{version_name}-retry-{attempt}");
        }
        if let Some(existing) = versions.into_iter().find(|v| v.name == import_name) {
            if existing.id.is_empty() {
                return Err(ManagerError::Service(anyhow::anyhow!(
                    "Coder version '{}' reported no id",
                    version_name
                )));
            }
            // Verify the version's import job succeeded. Rediscover by stable
            // identity; never adopt a same-name version from another template.
            let job_id = existing
                .job
                .as_ref()
                .map(|j| j.id.clone())
                .unwrap_or_default();
            if job_id.is_empty() {
                return Err(ManagerError::api(
                    "IMPORT_JOB_UNKNOWN",
                    "existing template version did not report an import job",
                ));
            }
            self.await_import(org, &existing.id)
                .await
                .map_err(|e| retryable_import_error(e, attempt))?;
            return Ok(TemplatePublicationResult {
                published: PublishedTemplate {
                    template_id: template_id.clone(),
                    version_id: coder_client::CoderTemplateVersionId::new(existing.id),
                    import_job_id: job_id,
                },
                outcome: PublishOutcome::Reconciled,
                template_name: template_name.to_string(),
                version_name: version_name.to_string(),
            });
        }

        // 3. Template exists but our deterministic version does not: upload a
        //    new version (idempotent by version name) and poll its import.
        let version = self
            .client
            .create_template_version_with_variables(
                org,
                Some(&template_id),
                &import_name,
                archive,
                &variables,
            )
            .await
            .map_err(ManagerError::from)?;
        let job_id = version
            .job
            .as_ref()
            .map(|j| j.id.clone())
            .unwrap_or_default();
        if job_id.is_empty() {
            return Err(ManagerError::api(
                "IMPORT_JOB_UNKNOWN",
                "created template version did not report an import job",
            ));
        }
        self.await_import(org, &version.id)
            .await
            .map_err(|e| retryable_import_error(e, attempt))?;
        Ok(TemplatePublicationResult {
            published: PublishedTemplate {
                template_id: template_id.clone(),
                version_id: coder_client::CoderTemplateVersionId::new(version.id),
                import_job_id: job_id,
            },
            outcome: PublishOutcome::Created,
            template_name: template_name.to_string(),
            version_name: version_name.to_string(),
        })
    }

    async fn verify_org_models(&self, org: &CoderOrganizationId) -> Result<bool, ManagerError> {
        let models = self
            .client
            .list_chat_models_in_org(org)
            .await
            .map_err(ManagerError::from)?;
        // The org-scoped endpoint reachable + at least one model means the org
        // has Agents/chat capability and a configured model.
        Ok(!models.is_empty())
    }

    async fn ensure_user_owner(
        &self,
        org: &CoderOrganizationId,
        owner: &crate::coder::types::CoderOwnerId,
    ) -> Result<(), ManagerError> {
        self.client
            .ensure_personal_org_member(org, &owner.0)
            .await
            .map_err(ManagerError::from)
    }

    async fn ensure_machine_owner(
        &self,
        org: &CoderOrganizationId,
        tenant_id: crate::id::TenantId,
    ) -> Result<crate::coder::types::CoderOwnerId, ManagerError> {
        let compact = tenant_id.0.simple().to_string();
        let username = format!("of-{}", &compact[..24]);
        let existing = self
            .client
            .get_user_detail(&username)
            .await
            .map_err(ManagerError::from)?;
        let user = match existing {
            Some(user) => user,
            None => {
                let request = coder_client::types::CreateServiceAccountRequest {
                    username: username.clone(),
                    name: format!("OpenFlows tenant {}", tenant_id.0),
                    // Coder v2.37 rejects email addresses on service accounts.
                    // Service accounts are non-login identities, so leave the
                    // optional email field empty.
                    email: String::new(),
                    login_type: "none".into(),
                    password: String::new(),
                    organization_ids: vec![org.0.clone()],
                    roles: Vec::new(),
                    service_account: true,
                    user_status: "active".into(),
                };
                match self.client.create_service_account(&request).await {
                    Ok(user) => user,
                    Err(create_error) => self
                        .client
                        .get_user_detail(&username)
                        .await
                        .map_err(ManagerError::from)?
                        .ok_or_else(|| ManagerError::from(create_error))?,
                }
            }
        };
        if user.id.is_empty()
            || !user.is_service_account
            || user.organization_ids.len() != 1
            || user.organization_ids.first() != Some(&org.0)
        {
            return Err(ManagerError::api(
                "CODER_SCOPE_MISMATCH",
                "tenant machine owner is not an exclusive organization service account",
            ));
        }
        Ok(crate::coder::types::CoderOwnerId::new(user.id))
    }

    async fn create_tenant_workspace(
        &self,
        org: &CoderOrganizationId,
        owner: &crate::coder::types::CoderOwnerId,
        template_id: &coder_client::CoderTemplateId,
        version_id: &coder_client::CoderTemplateVersionId,
        name: &str,
    ) -> Result<coder_client::CoderWorkspaceDetail, ManagerError> {
        // Explicit org/owner/template/version — never the default org or the
        // current operator user.
        self.client
            .create_workspace_explicit(
                &owner.0,
                &coder_client::types::CreateWorkspaceRequestExplicit {
                    organization_id: org.0.clone(),
                    template_id: template_id.0.clone(),
                    template_version_id: version_id.0.clone(),
                    name: name.to_string(),
                    rich_parameter_values: None,
                },
            )
            .await
            .map_err(ManagerError::from)
    }

    async fn create_tenant_workspace_with_parameters(
        &self,
        org: &CoderOrganizationId,
        owner: &crate::coder::types::CoderOwnerId,
        template_id: &coder_client::CoderTemplateId,
        version_id: &coder_client::CoderTemplateVersionId,
        name: &str,
        parameters: &[coder_client::types::WorkspaceParameterValue],
    ) -> Result<coder_client::CoderWorkspaceDetail, ManagerError> {
        if let Some(existing) = self
            .client
            .get_workspace_by_owner_and_name(&owner.0, name)
            .await
            .map_err(ManagerError::from)?
        {
            verify_workspace_scope(&existing, org, owner, template_id, version_id, name)?;
            return Ok(existing);
        }
        let request = coder_client::types::CreateWorkspaceRequestExplicit {
            organization_id: org.0.clone(),
            template_id: template_id.0.clone(),
            template_version_id: version_id.0.clone(),
            name: name.to_string(),
            rich_parameter_values: Some(parameters.to_vec()),
        };
        let workspace = match self
            .client
            .create_workspace_explicit(&owner.0, &request)
            .await
        {
            Ok(workspace) => workspace,
            Err(create_error) => {
                tracing::warn!(
                    organization_id = %org.0,
                    owner_id = %owner.0,
                    template_id = %template_id.0,
                    version_id = %version_id.0,
                    workspace_name = name,
                    error = %create_error,
                    "Coder rejected tenant workspace creation"
                );
                self.client
                    .get_workspace_by_owner_and_name(&owner.0, name)
                    .await
                    .map_err(ManagerError::from)?
                    .ok_or_else(|| ManagerError::from(create_error))?
            }
        };
        verify_workspace_scope(&workspace, org, owner, template_id, version_id, name)?;
        Ok(workspace)
    }

    async fn wait_workspace_ready(
        &self,
        org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        // Poll the workspace until its latest build job succeeds and the agent
        // connects. This requires a live deployment to contract against; the
        // real readiness semantics are an operator gate.
        let deadline = tokio::time::Instant::now() + IMPORT_POLL_TIMEOUT;
        loop {
            let ws = self
                .client
                .get_workspace(workspace_id)
                .await
                .map_err(ManagerError::from)?;
            if ws.is_running() {
                return Ok(());
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ManagerError::api(
                    "TENANT_WS_NOT_READY",
                    format!(
                        "workspace {workspace_id} did not become ready in org {} within timeout",
                        org.0
                    ),
                ));
            }
            tokio::time::sleep(IMPORT_POLL_INTERVAL).await;
        }
    }

    async fn delete_workspace(
        &self,
        _org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        self.client
            .delete_workspace(workspace_id)
            .await
            .map_err(ManagerError::from)
    }

    async fn start_workspace(
        &self,
        _org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        self.client
            .start_workspace(workspace_id)
            .await
            .map(|_| ())
            .map_err(ManagerError::from)
    }

    async fn restart_workspace(
        &self,
        _org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        self.client
            .restart_workspace(workspace_id)
            .await
            .map_err(ManagerError::from)
    }

    async fn stop_workspace(
        &self,
        _org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        self.client
            .stop_workspace(workspace_id)
            .await
            .map_err(ManagerError::from)
    }

    async fn delete_machine_owner(
        &self,
        org: &CoderOrganizationId,
        owner: &crate::coder::types::CoderOwnerId,
    ) -> Result<(), ManagerError> {
        let Some(user) = self
            .client
            .get_user_detail(&owner.0)
            .await
            .map_err(ManagerError::from)?
        else {
            return Ok(());
        };
        if !user.is_service_account
            || user.organization_ids.len() != 1
            || user.organization_ids.first() != Some(&org.0)
        {
            return Err(ManagerError::api(
                "CODER_SCOPE_MISMATCH",
                "refusing to delete a non-exclusive Coder service account",
            ));
        }
        self.client
            .delete_user(&owner.0)
            .await
            .map_err(ManagerError::from)
    }
}

fn verify_workspace_scope(
    workspace: &coder_client::CoderWorkspaceDetail,
    org: &CoderOrganizationId,
    owner: &crate::coder::types::CoderOwnerId,
    template: &coder_client::CoderTemplateId,
    version: &coder_client::CoderTemplateVersionId,
    name: &str,
) -> Result<(), ManagerError> {
    if workspace.id.is_empty()
        || workspace.organization_id != org.0
        || workspace.owner_id != owner.0
        || workspace.template_id != template.0
        || workspace.template_version_id != version.0
        || workspace.name != name
    {
        return Err(ManagerError::api(
            "CODER_SCOPE_MISMATCH",
            "existing workspace does not match the expected tenant identity",
        ));
    }
    Ok(())
}

fn daemon_is_online(daemon: &coder_client::CoderProvisionerDaemon, key_name: &str) -> bool {
    daemon.key_name.as_deref() == Some(key_name)
        && matches!(daemon.status.as_deref(), Some("idle" | "busy"))
}

impl RealCoderAdapter {
    /// Bounded polling of a template version's import job until it succeeds. A failed or timed
    /// out import is a hard error: readiness must be prevented.
    async fn await_import(
        &self,
        org: &CoderOrganizationId,
        version_id: &str,
    ) -> Result<(), ManagerError> {
        let deadline = tokio::time::Instant::now() + TEMPLATE_IMPORT_TIMEOUT;
        loop {
            let job = self
                .client
                .get_template_version(org, version_id)
                .await
                .map_err(ManagerError::from)?;
            match job {
                Some(version) if version.job.as_ref().is_some_and(|job| job.is_succeeded()) => {
                    return Ok(())
                }
                Some(version) if version.job.as_ref().is_some_and(|job| job.is_terminal()) => {
                    return Err(ManagerError::api(
                        if version
                            .job
                            .as_ref()
                            .is_some_and(|job| job.status == "failed")
                        {
                            "IMPORT_FAILED"
                        } else {
                            "IMPORT_CANCELLED"
                        },
                        "template import job failed or was cancelled",
                    ));
                }
                _ => {}
            }
            if tokio::time::Instant::now() >= deadline {
                return Err(ManagerError::api(
                    "IMPORT_TIMEOUT",
                    "template import job did not succeed within timeout",
                ));
            }
            tokio::time::sleep(IMPORT_POLL_INTERVAL).await;
        }
    }

    /// Probe license + org-scoped capability by reading buildinfo version and
    /// listing organizations. Conservative: cannot be true without an org we
    /// can reach.
    async fn probe_license(&self) -> bool {
        self.client.list_organizations().await.is_ok()
    }
}
