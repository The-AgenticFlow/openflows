//! A deterministic in-memory Coder adapter for offline tests.
//!
//! It models the same resource state machine as a real Coder deployment
//! (organizations, templates, versions, import jobs, provisioners, models) so
//! the provisioning acceptance behaviors can be exercised without a licensed
//! deployment: cross-org name isolation, failed-import preventing readiness,
//! duplicate-publication retry convergence, and version-pin survival. It never
//! fabricates credentials or licenses; capability flags are explicit test
//! inputs.

use super::adapter::CoderAdapter;
use super::types::{
    CoderCapabilities, PublishOutcome, PublishedTemplate, TemplatePublicationResult,
};
use crate::error::ManagerError;
use async_trait::async_trait;
use coder_client::{CoderOrganizationId, CoderTemplateId, CoderTemplateVersionId};
use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

/// One simulated published template version within an organization.
#[derive(Debug, Clone)]
pub struct MockVersion {
    pub template_id: String,
    pub version_id: String,
    pub version_name: String,
    /// Deterministic import outcome for this version.
    pub import_status: String,
}

/// Internal simulated Coder state.
#[derive(Debug, Default)]
struct State {
    org_display_names: HashMap<String, String>,
    orgs: HashMap<String, String>,            // internal_name -> org id
    templates: HashMap<String, MockTemplate>, // template id -> template
    /// org id -> set of template ids
    org_templates: HashMap<String, HashSet<String>>,
    /// template id -> versions
    template_versions: HashMap<String, Vec<MockVersion>>,
    provisioner_ready: HashSet<String>, // org id
    models_ok: HashSet<String>,         // org id
    next_template_id: u64,
    next_version_id: u64,
    /// org id -> set of machine-owner ids
    machine_owners: HashMap<String, HashSet<String>>,
    /// workspace id -> simulated workspace
    workspaces: HashMap<String, MockWorkspace>,
    next_workspace_id: u64,
}

#[derive(Debug, Clone)]
struct MockTemplate {
    id: String,
    org_id: String,
    name: String,
    active_version_id: Option<String>,
}

/// A simulated tenant workspace in the mock Coder deployment.
#[derive(Debug, Clone)]
#[allow(dead_code)]
struct MockWorkspace {
    id: String,
    name: String,
    org_id: String,
    owner_id: String,
    template_id: String,
    version_id: String,
    running: bool,
}

/// Configuration knobs a test sets to drive failure and success scenarios.
#[derive(Debug, Clone)]
pub struct MockCoderConfig {
    pub license_ok: bool,
    pub version_ok: bool,
    pub provisioner_ready: bool,
    pub models_ok: bool,
    /// When set, template imports fail with this status instead of succeeding.
    pub import_status_override: Option<String>,
    /// When true, `ensure_organization` fabricates nothing but creation of an
    /// org is rejected (simulates insufficient license).
    pub organization_creation_denied: bool,
    /// When false, tenant workspaces never become ready (simulates a build
    /// that never reaches running).
    pub tenant_workspace_ready: bool,
    /// When true, machine-owner creation is rejected (simulates missing
    /// Premium/service-account capability).
    pub machine_owner_denied: bool,
}

impl Default for MockCoderConfig {
    fn default() -> Self {
        Self {
            license_ok: true,
            version_ok: true,
            provisioner_ready: true,
            models_ok: true,
            import_status_override: None,
            organization_creation_denied: false,
            tenant_workspace_ready: true,
            machine_owner_denied: false,
        }
    }
}

#[derive(Clone)]
pub struct MockCoderAdapter {
    state: Arc<Mutex<State>>,
    config: Arc<Mutex<MockCoderConfig>>,
}

impl Default for MockCoderAdapter {
    fn default() -> Self {
        MockCoderAdapter {
            state: Arc::new(Mutex::new(State::default())),
            config: Arc::new(Mutex::new(MockCoderConfig {
                license_ok: true,
                version_ok: true,
                provisioner_ready: true,
                models_ok: true,
                import_status_override: None,
                organization_creation_denied: false,
                tenant_workspace_ready: true,
                machine_owner_denied: false,
            })),
        }
    }
}

impl MockCoderAdapter {
    pub fn organization_display_name(&self, org: &CoderOrganizationId) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .org_display_names
            .get(&org.0)
            .cloned()
    }
    pub fn set_active_version(&self, template_id: &str, version_id: &str) {
        let mut state = self.state.lock().unwrap();
        assert!(state
            .template_versions
            .get(template_id)
            .unwrap()
            .iter()
            .any(|v| v.version_id == version_id));
        state
            .templates
            .get_mut(template_id)
            .unwrap()
            .active_version_id = Some(version_id.to_string());
    }

    pub fn active_version(&self, template_id: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .templates
            .get(template_id)
            .and_then(|t| t.active_version_id.clone())
    }
    pub fn new(config: MockCoderConfig) -> Self {
        MockCoderAdapter {
            state: Arc::new(Mutex::new(State::default())),
            config: Arc::new(Mutex::new(config)),
        }
    }

    pub fn set_import_status(&self, status: Option<&str>) {
        let mut c = self.config.lock().unwrap();
        c.import_status_override = status.map(|s| s.to_string());
    }

    /// The number of versions currently recorded for a (org, template). Used to
    /// assert duplicate-publication retries do not append unbounded versions.
    pub fn version_count(&self, org: &CoderOrganizationId, template_name: &str) -> usize {
        let state = self.state.lock().unwrap();
        let template = state
            .templates
            .values()
            .find(|t| t.org_id == org.0 && t.name == template_name);
        match template {
            Some(t) => state
                .template_versions
                .get(&t.id)
                .map(|v| v.len())
                .unwrap_or(0),
            None => 0,
        }
    }
}

#[async_trait]
impl CoderAdapter for MockCoderAdapter {
    async fn verify_capabilities(&self) -> Result<CoderCapabilities, ManagerError> {
        let c = self.config.lock().unwrap();
        Ok(CoderCapabilities {
            version_ok: c.version_ok,
            license_ok: c.license_ok,
            agents_chat_ok: c.models_ok,
        })
    }

    async fn ensure_organization(
        &self,
        internal_name: &str,
        display_name: &str,
    ) -> Result<CoderOrganizationId, ManagerError> {
        if self.config.lock().unwrap().organization_creation_denied {
            return Err(ManagerError::api(
                "ORGANIZATION_CREATE_DENIED",
                "organization creation denied (license prerequisite missing)",
            ));
        }
        let mut state = self.state.lock().unwrap();
        if let Some(id) = state.orgs.get(internal_name) {
            return Ok(CoderOrganizationId::new(id.clone()));
        }
        let id = internal_name.to_string();
        state
            .org_display_names
            .insert(id.clone(), display_name.to_string());
        state.orgs.insert(internal_name.to_string(), id.clone());
        Ok(CoderOrganizationId::new(id))
    }

    async fn ensure_provisioner_ready(
        &self,
        org: &CoderOrganizationId,
        _backend: &str,
    ) -> Result<(), ManagerError> {
        let ready = self.config.lock().unwrap().provisioner_ready;
        if !ready {
            return Err(ManagerError::api(
                "PROVISIONER_NOT_READY",
                "provisioner not ready (no heartbeat)",
            ));
        }
        self.state
            .lock()
            .unwrap()
            .provisioner_ready
            .insert(org.0.clone());
        Ok(())
    }

    async fn ensure_template_version(
        &self,
        org: &CoderOrganizationId,
        template_name: &str,
        version_name: &str,
        _archive: &[u8],
    ) -> Result<TemplatePublicationResult, ManagerError> {
        let import_status = self
            .config
            .lock()
            .unwrap()
            .import_status_override
            .clone()
            .unwrap_or_else(|| "succeeded".to_string());

        // Resolve or create the template for this org+name deterministically,
        // computing ids up front to avoid aliasing the borrow of `state`.
        let template_id;
        {
            let mut state = self.state.lock().unwrap();
            let existing = state
                .templates
                .values()
                .find(|t| t.org_id == org.0 && t.name == template_name)
                .map(|t| t.id.clone());
            template_id = match existing {
                Some(id) => id,
                None => {
                    state.next_template_id += 1;
                    let id = format!("tpl-{}-{}", org.0, state.next_template_id);
                    let t = MockTemplate {
                        id: id.clone(),
                        org_id: org.0.clone(),
                        name: template_name.to_string(),
                        active_version_id: None,
                    };
                    state.templates.insert(id.clone(), t);
                    state
                        .org_templates
                        .entry(org.0.clone())
                        .or_default()
                        .insert(id.clone());
                    id
                }
            };
        }

        // Reconcile against an existing version with our deterministic name.
        {
            let mut state = self.state.lock().unwrap();
            let versions = state
                .template_versions
                .entry(template_id.clone())
                .or_default();
            if let Some(existing) = versions.iter().find(|v| v.version_name == version_name) {
                if existing.import_status == "succeeded" {
                    return Ok(TemplatePublicationResult {
                        published: PublishedTemplate {
                            template_id: CoderTemplateId::new(template_id),
                            version_id: CoderTemplateVersionId::new(existing.version_id.clone()),
                            import_job_id: format!("job-{}", existing.version_id),
                        },
                        outcome: PublishOutcome::Reconciled,
                        template_name: template_name.to_string(),
                        version_name: version_name.to_string(),
                    });
                }
                return Err(ManagerError::api(
                    "IMPORT_FAILED",
                    "template import job failed or was cancelled",
                ));
            }
        }

        // Create a new version with the given import outcome.
        let version_id;
        {
            let mut state = self.state.lock().unwrap();
            state.next_version_id += 1;
            version_id = format!("ver-{}-{}", template_id, state.next_version_id);
            let v = MockVersion {
                template_id: template_id.clone(),
                version_id: version_id.clone(),
                version_name: version_name.to_string(),
                import_status: import_status.clone(),
            };
            state
                .template_versions
                .entry(template_id.clone())
                .or_default()
                .push(v);
            let template = state.templates.get_mut(&template_id).unwrap();
            if template.active_version_id.is_none() {
                template.active_version_id = Some(version_id.clone());
            }
        }

        if import_status != "succeeded" {
            return Err(ManagerError::api(
                "IMPORT_FAILED",
                "template import job failed or was cancelled",
            ));
        }

        Ok(TemplatePublicationResult {
            published: PublishedTemplate {
                template_id: CoderTemplateId::new(template_id),
                version_id: CoderTemplateVersionId::new(version_id.clone()),
                import_job_id: format!("job-{version_id}"),
            },
            outcome: PublishOutcome::Created,
            template_name: template_name.to_string(),
            version_name: version_name.to_string(),
        })
    }

    async fn verify_org_models(&self, org: &CoderOrganizationId) -> Result<bool, ManagerError> {
        let ok = self.config.lock().unwrap().models_ok;
        if ok {
            self.state.lock().unwrap().models_ok.insert(org.0.clone());
        }
        Ok(ok)
    }

    async fn ensure_user_owner(
        &self,
        _org: &CoderOrganizationId,
        _owner: &crate::coder::types::CoderOwnerId,
    ) -> Result<(), ManagerError> {
        if self.config.lock().unwrap().machine_owner_denied {
            return Err(ManagerError::api(
                "CODER_USER_OWNER_FAILED",
                "linked owner unavailable",
            ));
        }
        Ok(())
    }

    async fn ensure_machine_owner(
        &self,
        org: &CoderOrganizationId,
        tenant_id: crate::id::TenantId,
    ) -> Result<crate::coder::types::CoderOwnerId, ManagerError> {
        if self.config.lock().unwrap().machine_owner_denied {
            return Err(ManagerError::api(
                "TENANT_RUNTIME_UNSUPPORTED",
                "machine-owner creation denied (Premium/service-account missing)",
            ));
        }
        let id = format!("owner-{}-{}", org.0, tenant_id.0);
        self.state
            .lock()
            .unwrap()
            .machine_owners
            .entry(org.0.clone())
            .or_default()
            .insert(id.clone());
        Ok(crate::coder::types::CoderOwnerId::new(id))
    }

    async fn create_tenant_workspace(
        &self,
        org: &CoderOrganizationId,
        owner: &crate::coder::types::CoderOwnerId,
        template_id: &CoderTemplateId,
        version_id: &CoderTemplateVersionId,
        name: &str,
    ) -> Result<coder_client::CoderWorkspaceDetail, ManagerError> {
        let running = self.config.lock().unwrap().tenant_workspace_ready;
        let mut state = self.state.lock().unwrap();
        if let Some(existing) = state
            .workspaces
            .values()
            .find(|workspace| workspace.owner_id == owner.0 && workspace.name == name)
        {
            if existing.org_id != org.0
                || existing.template_id != template_id.0
                || existing.version_id != version_id.0
            {
                return Err(ManagerError::api(
                    "CODER_SCOPE_MISMATCH",
                    "same-name mock workspace has incompatible ownership",
                ));
            }
            return Ok(coder_client::CoderWorkspaceDetail {
                id: existing.id.clone(),
                name: existing.name.clone(),
                owner_id: existing.owner_id.clone(),
                organization_id: existing.org_id.clone(),
                template_id: existing.template_id.clone(),
                template_version_id: existing.version_id.clone(),
            });
        }
        state.next_workspace_id += 1;
        let id = format!("ws-{}-{}", org.0, state.next_workspace_id);
        state.workspaces.insert(
            id.clone(),
            MockWorkspace {
                id: id.clone(),
                name: name.to_string(),
                org_id: org.0.clone(),
                owner_id: owner.0.clone(),
                template_id: template_id.0.clone(),
                version_id: version_id.0.clone(),
                running,
            },
        );
        Ok(coder_client::CoderWorkspaceDetail {
            id: id.clone(),
            name: name.to_string(),
            owner_id: owner.0.clone(),
            organization_id: org.0.clone(),
            template_id: template_id.0.clone(),
            template_version_id: version_id.0.clone(),
        })
    }

    async fn wait_workspace_ready(
        &self,
        org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        let state = self.state.lock().unwrap();
        match state.workspaces.get(workspace_id) {
            Some(ws) if ws.running && ws.org_id == org.0 => Ok(()),
            _ => Err(ManagerError::api(
                "TENANT_WS_NOT_READY",
                format!("workspace {workspace_id} is not ready"),
            )),
        }
    }

    async fn delete_workspace(
        &self,
        org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        let mut state = self.state.lock().unwrap();
        match state.workspaces.get(workspace_id) {
            Some(ws) if ws.org_id == org.0 => {
                state.workspaces.remove(workspace_id);
                Ok(())
            }
            None => Ok(()),
            _ => Err(ManagerError::api(
                "CODER_SCOPE_MISMATCH",
                "workspace belongs to another organization",
            )),
        }
    }

    async fn start_workspace(
        &self,
        org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        let mut state = self.state.lock().unwrap();
        let workspace = state
            .workspaces
            .get_mut(workspace_id)
            .filter(|workspace| workspace.org_id == org.0)
            .ok_or_else(|| ManagerError::not_found("workspace"))?;
        workspace.running = true;
        Ok(())
    }

    async fn restart_workspace(
        &self,
        org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        // The mock has no separate build history; a restart leaves the
        // workspace in its configured ready state.
        self.start_workspace(org, workspace_id).await
    }

    async fn stop_workspace(
        &self,
        org: &CoderOrganizationId,
        workspace_id: &str,
    ) -> Result<(), ManagerError> {
        let mut state = self.state.lock().unwrap();
        let workspace = state
            .workspaces
            .get_mut(workspace_id)
            .filter(|workspace| workspace.org_id == org.0)
            .ok_or_else(|| ManagerError::not_found("workspace"))?;
        workspace.running = false;
        Ok(())
    }

    async fn delete_machine_owner(
        &self,
        org: &CoderOrganizationId,
        owner: &crate::coder::types::CoderOwnerId,
    ) -> Result<(), ManagerError> {
        let mut state = self.state.lock().unwrap();
        if state
            .workspaces
            .values()
            .any(|workspace| workspace.owner_id == owner.0)
        {
            return Err(ManagerError::api(
                "CODER_OWNER_IN_USE",
                "machine owner still owns a workspace",
            ));
        }
        if let Some(owners) = state.machine_owners.get_mut(&org.0) {
            owners.remove(&owner.0);
        }
        Ok(())
    }
}

impl MockCoderAdapter {
    /// Whether a machine owner exists for `org` in the mock.
    pub fn has_machine_owner(&self, org: &CoderOrganizationId) -> bool {
        self.state
            .lock()
            .unwrap()
            .machine_owners
            .get(&org.0)
            .map(|set| !set.is_empty())
            .unwrap_or(false)
    }

    /// The machine owner id recorded for a tenant in `org`, if any.
    pub fn machine_owner(
        &self,
        org: &CoderOrganizationId,
        tenant_id: &crate::id::TenantId,
    ) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .machine_owners
            .get(&org.0)
            .and_then(|set| {
                set.iter()
                    .find(|o| o.contains(&tenant_id.0.to_string()))
                    .cloned()
            })
    }

    /// Whether a workspace exists for a given owner in `org`.
    pub fn has_workspace_for_owner(&self, org: &CoderOrganizationId, owner: &str) -> bool {
        self.state
            .lock()
            .unwrap()
            .workspaces
            .values()
            .any(|w| w.org_id == org.0 && w.owner_id == owner)
    }

    /// The version id recorded for the first workspace in `org` owned by
    /// `owner`, used to assert pinned template/version selection.
    pub fn workspace_version(&self, org: &CoderOrganizationId, owner: &str) -> Option<String> {
        self.state
            .lock()
            .unwrap()
            .workspaces
            .values()
            .find(|w| w.org_id == org.0 && w.owner_id == owner)
            .map(|w| w.version_id.clone())
    }
}
