//! Tenant lifecycle service shared across CLI and API.

use crate::{
    error::ManagerError,
    models::tenant::{
        TenantCleanResponse, TenantCreateRequest, TenantCreateResponse, TenantDetail,
        TenantRemoveResponse, TenantSummary,
    },
};
use pocketflow_core::SharedStore;
use std::{collections::HashSet, sync::Arc};
use tracing::info;

/// Embedded default registry JSON if no on-disk override is found.
pub const EMBEDDED_REGISTRY_JSON: &str =
    include_str!("../../../../orchestration/agent/registry.json");

/// Validate that a tenant name conforms to OpenFlows and Redis naming standards.
pub fn validate_tenant_name(name: &str) -> Result<(), ManagerError> {
    if name.is_empty() {
        return Err(ManagerError::InvalidTenantName(
            "tenant name must not be empty".to_string(),
        ));
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
    {
        return Err(ManagerError::InvalidTenantName(format!(
            "tenant name '{name}' contains characters that are not allowed in Redis namespace operations; use only ASCII letters, numbers, '.', '_' and '-'"
        )));
    }
    Ok(())
}

/// Validate that a GitHub repository is in the `owner/repo` format.
pub fn validate_repository(repo: &str) -> Result<(), ManagerError> {
    let parts: Vec<&str> = repo.split('/').collect();
    if parts.len() != 2 || parts[0].trim().is_empty() || parts[1].trim().is_empty() {
        return Err(ManagerError::InvalidRepository(format!(
            "repository must be in 'owner/repo' format, got '{repo}'"
        )));
    }
    Ok(())
}

/// Abstract provisioner for tenant workspace creation.
#[async_trait::async_trait]
pub trait TenantProvisioner: Send + Sync {
    async fn provision_tenant(
        &self,
        tenant_name: &str,
        repo: &str,
        registry_json: &str,
    ) -> Result<String, ManagerError>;
}

/// Live Coder workspace provisioner.
pub struct CoderTenantProvisioner;

#[async_trait::async_trait]
impl TenantProvisioner for CoderTenantProvisioner {
    async fn provision_tenant(
        &self,
        tenant_name: &str,
        repo: &str,
        registry_json: &str,
    ) -> Result<String, ManagerError> {
        let bootstrapper = coder_client::bootstrap::CoderBootstrapper::from_env()
            .map_err(|e| ManagerError::Config(format!("Coder configuration error: {e}")))?;

        let client = bootstrapper
            .bootstrap()
            .await
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("Coder bootstrap error: {e}")))?;

        let workspace_id = bootstrapper
            .ensure_tenant(&client, tenant_name, repo, registry_json)
            .await
            .map_err(|e| {
                ManagerError::Service(anyhow::anyhow!("Coder tenant setup failed: {e}"))
            })?;

        Ok(workspace_id)
    }
}

/// Test/Mock provisioner that generates mock workspace IDs without contacting Coder.
pub struct MockTenantProvisioner;

#[async_trait::async_trait]
impl TenantProvisioner for MockTenantProvisioner {
    async fn provision_tenant(
        &self,
        tenant_name: &str,
        _repo: &str,
        _registry_json: &str,
    ) -> Result<String, ManagerError> {
        Ok(format!("ws-mock-nexus-{}", tenant_name))
    }
}

/// Reusable tenant lifecycle service.
#[derive(Clone)]
pub struct TenantService {
    store: SharedStore,
    provisioner: Arc<dyn TenantProvisioner>,
}

impl TenantService {
    pub fn new(store: SharedStore, provisioner: Arc<dyn TenantProvisioner>) -> Self {
        Self { store, provisioner }
    }

    pub fn with_coder_provisioner(store: SharedStore) -> Self {
        Self::new(store, Arc::new(CoderTenantProvisioner))
    }

    pub fn for_tests(store: SharedStore) -> Self {
        Self::new(store, Arc::new(MockTenantProvisioner))
    }

    /// List all tenants found across Redis namespaces.
    pub async fn list_tenants(&self) -> Result<Vec<TenantSummary>, ManagerError> {
        let keys: Vec<String> = self.store.raw_keys("ns:*").await;
        let mut tenant_names = HashSet::new();
        for key in keys {
            if let Some(ns) = key.strip_prefix("ns:") {
                if let Some(tenant) = ns.split(':').next() {
                    if !tenant.is_empty() {
                        tenant_names.insert(tenant.to_string());
                    }
                }
            }
        }

        let mut list: Vec<TenantSummary> = Vec::with_capacity(tenant_names.len());
        for name in tenant_names {
            let t_store = self.store.for_tenant(&name);
            let repository: Option<String> = t_store.get_typed("repository").await;
            list.push(TenantSummary { name, repository });
        }
        list.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(list)
    }

    /// Retrieve detailed state for a specific tenant.
    pub async fn get_tenant(&self, tenant: &str) -> Result<TenantDetail, ManagerError> {
        validate_tenant_name(tenant)?;

        let pattern = format!("ns:{}:*", tenant);
        let keys: Vec<String> = self.store.raw_keys(&pattern).await;
        if keys.is_empty() {
            return Err(ManagerError::TenantNotFound(tenant.to_string()));
        }

        let t_store = self.store.for_tenant(tenant);
        let repository: Option<String> = t_store.get_typed("repository").await;
        let registry: Option<serde_json::Value> = t_store.get("registry_json").await;

        let tickets: Vec<serde_json::Value> =
            t_store.get_typed("tickets").await.unwrap_or_default();
        let ticket_count = tickets.len();

        let worker_slots: serde_json::Map<String, serde_json::Value> =
            t_store.get_typed("worker_slots").await.unwrap_or_default();

        let active_workers = worker_slots
            .values()
            .filter(|slot| {
                slot.get("status")
                    .and_then(|s| s.get("type"))
                    .and_then(|t| t.as_str())
                    .map(|t| t == "assigned" || t == "working")
                    .unwrap_or(false)
            })
            .count();

        Ok(TenantDetail {
            name: tenant.to_string(),
            repository,
            registry,
            key_count: keys.len(),
            ticket_count,
            active_workers,
        })
    }

    /// Create and initialize a new tenant environment.
    pub async fn create_tenant(
        &self,
        req: TenantCreateRequest,
    ) -> Result<TenantCreateResponse, ManagerError> {
        validate_repository(&req.repo)?;
        let tenant_name = req
            .name
            .clone()
            .unwrap_or_else(|| req.repo.split('/').next().unwrap_or(&req.repo).to_string());
        validate_tenant_name(&tenant_name)?;

        if req.fleet < 1 {
            return Err(ManagerError::InvalidRequest(format!(
                "fleet must be at least 1, got {}",
                req.fleet
            )));
        }

        // Verify Redis backend is reachable
        self.store
            .ping()
            .await
            .map_err(|e| ManagerError::Store(format!("Redis connection failed: {e}")))?;

        // Check if tenant already has persisted state or active reservation
        let existing_keys = self.store.raw_keys(&format!("ns:{}:*", tenant_name)).await;
        if !existing_keys.is_empty() {
            return Err(ManagerError::TenantAlreadyExists(tenant_name));
        }

        // Reserve tenant name in Redis before provisioning
        let t_store = self.store.for_tenant(&tenant_name);
        t_store
            .set("repository", serde_json::json!(&req.repo))
            .await;
        if t_store.get("repository").await.is_none() {
            return Err(ManagerError::Store(
                "Failed to reserve tenant: write to repository key failed".to_string(),
            ));
        }

        // Load base registry and apply fleet size
        let base_registry = match self.load_base_registry() {
            Ok(reg) => reg,
            Err(e) => {
                t_store.del("repository").await;
                return Err(e);
            }
        };
        let tenant_registry = base_registry.with_team_fleet(req.fleet);
        let registry_json = match serde_json::to_string_pretty(&tenant_registry) {
            Ok(j) => j,
            Err(e) => {
                t_store.del("repository").await;
                return Err(ManagerError::Service(anyhow::anyhow!(
                    "Failed to serialize registry: {e}"
                )));
            }
        };

        // Provision workspace via provisioner
        let workspace_id = match self
            .provisioner
            .provision_tenant(&tenant_name, &req.repo, &registry_json)
            .await
        {
            Ok(wid) => wid,
            Err(e) => {
                // Roll back reservation on provisioning failure
                t_store.del("repository").await;
                return Err(e);
            }
        };

        // Persist registry into the tenant store
        t_store
            .set("registry_json", serde_json::json!(&registry_json))
            .await;
        if t_store.get("registry_json").await.is_none() {
            t_store.del("repository").await;
            return Err(ManagerError::Store(
                "Failed to persist tenant registry: write failed".to_string(),
            ));
        }

        info!(
            tenant = %tenant_name,
            repo = %req.repo,
            fleet = req.fleet,
            workspace_id = %workspace_id,
            "Tenant provisioned successfully"
        );

        Ok(TenantCreateResponse {
            tenant: tenant_name,
            repository: req.repo,
            fleet: req.fleet,
            workspace_id,
            message: "Tenant created successfully".to_string(),
        })
    }

    /// Reset/clean a tenant's runtime state.
    pub async fn clean_tenant(
        &self,
        tenant: &str,
        reset_all: bool,
    ) -> Result<TenantCleanResponse, ManagerError> {
        validate_tenant_name(tenant)?;

        self.store
            .ping()
            .await
            .map_err(|e| ManagerError::Store(format!("Redis connection failed: {e}")))?;

        let pattern = format!("ns:{}:*", tenant);
        let keys = self.store.raw_keys(&pattern).await;
        if keys.is_empty() {
            return Err(ManagerError::TenantNotFound(tenant.to_string()));
        }

        let t_store = self.store.for_tenant(tenant);
        let mut tickets: Vec<serde_json::Value> =
            t_store.get_typed("tickets").await.unwrap_or_default();
        let mut reset_count = 0;

        for ticket in tickets.iter_mut() {
            let status = ticket.get("status");
            let is_stale = status
                .map(|s| {
                    let stype = match s {
                        serde_json::Value::Object(obj) => {
                            obj.get("type").and_then(|v| v.as_str()).unwrap_or("")
                        }
                        serde_json::Value::String(s) => s.as_str(),
                        _ => "",
                    };
                    stype == "awaiting_human" || stype == "failed"
                })
                .unwrap_or(false);

            if is_stale || reset_all {
                ticket["status"] = serde_json::json!({
                    "type": "open"
                });
                if let Some(obj) = ticket.as_object_mut() {
                    obj.insert("attempts".to_string(), serde_json::json!(0));
                }
                if let Some(tid) = ticket.get("id").and_then(|v| v.as_str()) {
                    t_store.del(&format!("ticket:{}:status", tid)).await;
                }
                reset_count += 1;
            }
        }

        if reset_count > 0 {
            t_store
                .set(
                    "tickets",
                    serde_json::to_value(&tickets).map_err(|e| ManagerError::Service(e.into()))?,
                )
                .await;
        }

        // Clear recovery attempt counters
        let recovery_pattern = format!("ns:{}:ticket:*:recovery_attempts", tenant);
        let recovery_keys = self.store.raw_keys(&recovery_pattern).await;
        let recovery_count = recovery_keys.len();
        for key in &recovery_keys {
            self.store.raw_del(key).await;
        }

        // Clear worker_slots
        t_store.set("worker_slots", serde_json::json!({})).await;

        Ok(TenantCleanResponse {
            tenant: tenant.to_string(),
            reset_tickets_count: reset_count,
            cleared_recovery_counters: recovery_count,
            message: format!("Tenant '{tenant}' cleaned successfully"),
        })
    }

    /// Remove a tenant and optionally purge all associated keys from Redis.
    pub async fn remove_tenant(
        &self,
        tenant: &str,
        purge: bool,
    ) -> Result<TenantRemoveResponse, ManagerError> {
        validate_tenant_name(tenant)?;

        self.store
            .ping()
            .await
            .map_err(|e| ManagerError::Store(format!("Redis connection failed: {e}")))?;

        let pattern = format!("ns:{}:*", tenant);
        let keys = self.store.raw_keys(&pattern).await;
        if keys.is_empty() {
            return Err(ManagerError::TenantNotFound(tenant.to_string()));
        }

        let mut purged_count = 0;
        if purge {
            for key in &keys {
                self.store.raw_del(key).await;
                purged_count += 1;
            }
        } else {
            // Non-purging deregistration: remove identity bindings so the tenant
            // is deregistered from tenant queries while preserving historical data.
            let t_store = self.store.for_tenant(tenant);
            t_store.del("repository").await;
            t_store.del("registry_json").await;
            t_store.del("workspace_id").await;
            purged_count = 3;
        }

        Ok(TenantRemoveResponse {
            tenant: tenant.to_string(),
            purged_keys_count: purged_count,
            message: format!("Tenant '{tenant}' removed successfully"),
        })
    }

    fn load_base_registry(&self) -> Result<config::Registry, ManagerError> {
        if let Ok(reg_path) = std::env::var("OPENFLOWS_REGISTRY_PATH") {
            let path = std::path::PathBuf::from(reg_path);
            if path.exists() {
                return config::Registry::load(&path).map_err(|e| {
                    ManagerError::Config(format!(
                        "Failed to load registry from OPENFLOWS_REGISTRY_PATH ({}): {e}",
                        path.display()
                    ))
                });
            }
        }

        let mut candidates: Vec<std::path::PathBuf> = Vec::new();
        if let Ok(home) = std::env::var("OPENFLOWS_HOME") {
            candidates.push(std::path::PathBuf::from(home));
        }
        if let Ok(exe) = std::env::current_exe() {
            if let Some(dir) = exe.parent() {
                if !dir.as_os_str().is_empty() {
                    candidates.push(dir.to_path_buf());
                }
                if let Some(parent) = dir.parent() {
                    if !parent.as_os_str().is_empty() {
                        candidates.push(parent.to_path_buf());
                    }
                }
            }
        }
        if let Ok(cwd) = std::env::current_dir() {
            candidates.push(cwd);
        }

        for dir in candidates {
            let dir_str = dir.to_string_lossy();
            if dir_str.ends_with("/orchestration/agent")
                || dir_str.ends_with("/orchestration")
                || dir_str.ends_with("/orchestration/agent/")
                || dir_str.ends_with("/orchestration/")
            {
                continue;
            }
            let reg_file = dir.join("orchestration/agent/registry.json");
            if reg_file.exists() {
                return config::Registry::load(&reg_file).map_err(|e| {
                    ManagerError::Config(format!(
                        "Failed to load on-disk registry at {}: {e}",
                        reg_file.display()
                    ))
                });
            }
        }

        // Fall back to embedded registry
        serde_json::from_str(EMBEDDED_REGISTRY_JSON).map_err(|e| {
            ManagerError::Config(format!("Failed to parse embedded default registry: {e}"))
        })
    }
}
