//! Service for AI Provider, Model management, and OpenFlows Model Assignment Policy (Issue #290).

use crate::{
    error::ManagerError,
    models::{
        AiModelCreateRequest, AiModelSummary, AiModelUpdateRequest, AiProviderCreateRequest,
        AiProviderSummary, AiProviderUpdateRequest, ModelPolicy, ModelResolutionSource,
        ResolvedModel,
    },
    services::validate_tenant_name,
};
use coder_client::{
    types::{
        CoderAiProvider, CoderChatModelConfig, CreateAiProviderRequest, CreateChatModelRequest,
        UpdateAiProviderRequest, UpdateChatModelRequest,
    },
    CoderClient,
};
use pocketflow_core::SharedStore;
use std::sync::Arc;
use tokio::sync::RwLock;

const GLOBAL_MODEL_POLICY_KEY: &str = "system:ai:model_policy";

/// Abstract backend interface for AI Provider and Model operations.
#[async_trait::async_trait]
pub trait AiBackend: Send + Sync {
    async fn list_providers(&self) -> Result<Vec<CoderAiProvider>, ManagerError>;
    async fn get_provider(&self, id_or_name: &str) -> Result<CoderAiProvider, ManagerError>;
    async fn create_provider(
        &self,
        req: &CreateAiProviderRequest,
    ) -> Result<CoderAiProvider, ManagerError>;
    async fn update_provider(
        &self,
        id_or_name: &str,
        req: &UpdateAiProviderRequest,
    ) -> Result<CoderAiProvider, ManagerError>;
    async fn delete_provider(&self, id_or_name: &str) -> Result<(), ManagerError>;

    async fn list_models(&self) -> Result<Vec<CoderChatModelConfig>, ManagerError>;
    async fn create_model(
        &self,
        req: &CreateChatModelRequest,
    ) -> Result<CoderChatModelConfig, ManagerError>;
    async fn update_model(
        &self,
        model_id: &str,
        req: &UpdateChatModelRequest,
    ) -> Result<CoderChatModelConfig, ManagerError>;
    async fn delete_model(&self, model_id: &str) -> Result<(), ManagerError>;
}

/// Live Coder AI backend.
pub struct CoderAiBackend {
    client: tokio::sync::RwLock<Option<CoderClient>>,
}

impl CoderAiBackend {
    pub fn new(client: CoderClient) -> Self {
        Self {
            client: tokio::sync::RwLock::new(Some(client)),
        }
    }

    pub fn from_env() -> Result<Self, ManagerError> {
        let env = config::EnvConfig::from_env()
            .map_err(|e| ManagerError::Config(format!("config error: {e}")))?;
        let url = env.coder.url.clone();
        if let Some(token) = env.coder.effective_token() {
            let mut client = CoderClient::new_unauthenticated(&url);
            client = client.with_token(token.clone()).with_session_token(&token);
            Ok(Self::new(client))
        } else {
            Ok(Self {
                client: tokio::sync::RwLock::new(None),
            })
        }
    }

    async fn get_client(&self) -> Result<CoderClient, ManagerError> {
        {
            let guard = self.client.read().await;
            if let Some(ref c) = *guard {
                return Ok(c.clone());
            }
        }
        let mut guard = self.client.write().await;
        if let Some(ref c) = *guard {
            return Ok(c.clone());
        }
        let bootstrapper = coder_client::bootstrap::CoderBootstrapper::from_env()
            .map_err(|e| ManagerError::Config(format!("Coder configuration error: {e}")))?;
        let client = bootstrapper
            .bootstrap()
            .await
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("Coder bootstrap error: {e}")))?;
        *guard = Some(client.clone());
        Ok(client)
    }
}

#[async_trait::async_trait]
impl AiBackend for CoderAiBackend {
    async fn list_providers(&self) -> Result<Vec<CoderAiProvider>, ManagerError> {
        self.get_client()
            .await?
            .list_ai_providers()
            .await
            .map_err(ManagerError::Service)
    }

    async fn get_provider(&self, id_or_name: &str) -> Result<CoderAiProvider, ManagerError> {
        let client = self.get_client().await?;
        match client.get_ai_provider(id_or_name).await {
            Ok(p) => Ok(p),
            Err(e) => {
                let err_str = e.to_string();
                if err_str.contains("404") || err_str.to_lowercase().contains("not found") {
                    Err(ManagerError::ProviderNotFound(id_or_name.to_string()))
                } else {
                    Err(ManagerError::Service(e))
                }
            }
        }
    }

    async fn create_provider(
        &self,
        req: &CreateAiProviderRequest,
    ) -> Result<CoderAiProvider, ManagerError> {
        self.get_client()
            .await?
            .create_ai_provider(req)
            .await
            .map_err(ManagerError::Service)
    }

    async fn update_provider(
        &self,
        id_or_name: &str,
        req: &UpdateAiProviderRequest,
    ) -> Result<CoderAiProvider, ManagerError> {
        self.get_client()
            .await?
            .update_ai_provider(id_or_name, req)
            .await
            .map_err(ManagerError::Service)
    }

    async fn delete_provider(&self, id_or_name: &str) -> Result<(), ManagerError> {
        self.get_client()
            .await?
            .delete_ai_provider(id_or_name)
            .await
            .map_err(ManagerError::Service)
    }

    async fn list_models(&self) -> Result<Vec<CoderChatModelConfig>, ManagerError> {
        self.get_client()
            .await?
            .list_chat_model_configs()
            .await
            .map_err(ManagerError::Service)
    }

    async fn create_model(
        &self,
        req: &CreateChatModelRequest,
    ) -> Result<CoderChatModelConfig, ManagerError> {
        self.get_client()
            .await?
            .create_chat_model(req)
            .await
            .map_err(ManagerError::Service)
    }

    async fn update_model(
        &self,
        model_id: &str,
        req: &UpdateChatModelRequest,
    ) -> Result<CoderChatModelConfig, ManagerError> {
        self.get_client()
            .await?
            .update_chat_model(model_id, req)
            .await
            .map_err(ManagerError::Service)
    }

    async fn delete_model(&self, model_id: &str) -> Result<(), ManagerError> {
        self.get_client()
            .await?
            .delete_chat_model(model_id)
            .await
            .map_err(ManagerError::Service)
    }
}

/// In-memory Mock AI backend for testing without live Coder dependencies.
#[derive(Default)]
pub struct MockAiBackend {
    providers: RwLock<Vec<CoderAiProvider>>,
    models: RwLock<Vec<CoderChatModelConfig>>,
}

impl MockAiBackend {
    pub fn new() -> Self {
        Self::default()
    }
}

#[async_trait::async_trait]
impl AiBackend for MockAiBackend {
    async fn list_providers(&self) -> Result<Vec<CoderAiProvider>, ManagerError> {
        let guard = self.providers.read().await;
        Ok(guard.clone())
    }

    async fn get_provider(&self, id_or_name: &str) -> Result<CoderAiProvider, ManagerError> {
        let guard = self.providers.read().await;
        guard
            .iter()
            .find(|p| p.id == id_or_name || p.name == id_or_name)
            .cloned()
            .ok_or_else(|| ManagerError::ProviderNotFound(id_or_name.to_string()))
    }

    async fn create_provider(
        &self,
        req: &CreateAiProviderRequest,
    ) -> Result<CoderAiProvider, ManagerError> {
        let mut guard = self.providers.write().await;
        let id = format!("prov-{}", guard.len() + 1);
        let provider = CoderAiProvider {
            id,
            provider_type: req.provider_type.clone(),
            name: req.name.clone(),
            display_name: req.display_name.clone().unwrap_or_else(|| req.name.clone()),
            icon: String::new(),
            enabled: req.enabled,
            base_url: req.base_url.clone().unwrap_or_default(),
            api_keys: req.api_keys.clone(),
            settings: req.settings.clone(),
            created_at: "2026-09-24T10:00:00Z".to_string(),
            updated_at: "2026-09-24T10:00:00Z".to_string(),
        };
        guard.push(provider.clone());
        Ok(provider)
    }

    async fn update_provider(
        &self,
        id_or_name: &str,
        req: &UpdateAiProviderRequest,
    ) -> Result<CoderAiProvider, ManagerError> {
        let mut guard = self.providers.write().await;
        let p = guard
            .iter_mut()
            .find(|p| p.id == id_or_name || p.name == id_or_name)
            .ok_or_else(|| ManagerError::ProviderNotFound(id_or_name.to_string()))?;

        if let Some(ref d) = req.display_name {
            p.display_name = d.clone();
        }
        if let Some(ref u) = req.base_url {
            p.base_url = u.clone();
        }
        if let Some(ref k) = req.api_keys {
            p.api_keys = k.clone();
        }
        if let Some(e) = req.enabled {
            p.enabled = e;
        }
        Ok(p.clone())
    }

    async fn delete_provider(&self, id_or_name: &str) -> Result<(), ManagerError> {
        let mut guard = self.providers.write().await;
        let pos = guard
            .iter()
            .position(|p| p.id == id_or_name || p.name == id_or_name)
            .ok_or_else(|| ManagerError::ProviderNotFound(id_or_name.to_string()))?;
        guard.remove(pos);
        Ok(())
    }

    async fn list_models(&self) -> Result<Vec<CoderChatModelConfig>, ManagerError> {
        let guard = self.models.read().await;
        Ok(guard.clone())
    }

    async fn create_model(
        &self,
        req: &CreateChatModelRequest,
    ) -> Result<CoderChatModelConfig, ManagerError> {
        let mut guard = self.models.write().await;
        let id = format!("model-{}", guard.len() + 1);
        let m = CoderChatModelConfig {
            id,
            organization_id: "default-org".to_string(),
            ai_provider_id: req.ai_provider_id.clone(),
            model: req.model.clone(),
            display_name: req
                .display_name
                .clone()
                .unwrap_or_else(|| req.model.clone()),
            enabled: req.enabled,
            is_default: req.is_default,
            context_limit: req.context_limit,
            compression_threshold: req.compression_threshold,
            created_at: "2026-09-24T10:00:00Z".to_string(),
            updated_at: "2026-09-24T10:00:00Z".to_string(),
        };
        guard.push(m.clone());
        Ok(m)
    }

    async fn update_model(
        &self,
        model_id: &str,
        req: &UpdateChatModelRequest,
    ) -> Result<CoderChatModelConfig, ManagerError> {
        let mut guard = self.models.write().await;
        let m = guard
            .iter_mut()
            .find(|m| m.id == model_id)
            .ok_or_else(|| ManagerError::ModelNotFound(model_id.to_string()))?;

        if let Some(ref d) = req.display_name {
            m.display_name = d.clone();
        }
        if let Some(c) = req.context_limit {
            m.context_limit = c;
        }
        if let Some(c) = req.compression_threshold {
            m.compression_threshold = Some(c);
        }
        if let Some(d) = req.is_default {
            m.is_default = d;
        }
        if let Some(e) = req.enabled {
            m.enabled = e;
        }
        Ok(m.clone())
    }

    async fn delete_model(&self, model_id: &str) -> Result<(), ManagerError> {
        let mut guard = self.models.write().await;
        let pos = guard
            .iter()
            .position(|m| m.id == model_id)
            .ok_or_else(|| ManagerError::ModelNotFound(model_id.to_string()))?;
        guard.remove(pos);
        Ok(())
    }
}

/// Service exposing unified AI Provider, Model, and Policy controls.
#[derive(Clone)]
pub struct AiService {
    store: SharedStore,
    backend: Arc<dyn AiBackend>,
}

impl AiService {
    pub fn new(store: SharedStore, backend: Arc<dyn AiBackend>) -> Self {
        Self { store, backend }
    }

    pub fn for_tests(store: SharedStore) -> Self {
        Self::new(store, Arc::new(MockAiBackend::new()))
    }

    // ── AI Provider Management ─────────────────────────────────────────────

    /// List all AI providers, masking secret keys.
    pub async fn list_providers(&self) -> Result<Vec<AiProviderSummary>, ManagerError> {
        let raw = self.backend.list_providers().await?;
        Ok(raw.into_iter().map(mask_provider_summary).collect())
    }

    /// Get a specific AI provider by ID or Name.
    pub async fn get_provider(&self, id_or_name: &str) -> Result<AiProviderSummary, ManagerError> {
        let raw = self.backend.get_provider(id_or_name).await?;
        Ok(mask_provider_summary(raw))
    }

    /// Create an AI provider. Secrets are write-only.
    pub async fn create_provider(
        &self,
        req: AiProviderCreateRequest,
    ) -> Result<AiProviderSummary, ManagerError> {
        if req.name.trim().is_empty() {
            return Err(ManagerError::InvalidRequest(
                "provider name must not be empty".to_string(),
            ));
        }

        let p_type = req.provider_type.trim().to_lowercase();
        let allowed_types = ["openai", "anthropic", "openai-compat", "google", "bedrock"];
        if !allowed_types.contains(&p_type.as_str()) {
            return Err(ManagerError::InvalidRequest(format!(
                "unsupported provider type '{}'; supported types: {}",
                p_type,
                allowed_types.join(", ")
            )));
        }

        let mut api_keys = req.api_keys.unwrap_or_default();
        if let Some(key) = req.api_key {
            if !key.trim().is_empty() {
                api_keys.push(key);
            }
        }

        let create_req = CreateAiProviderRequest {
            provider_type: p_type,
            name: req.name.trim().to_string(),
            display_name: req.display_name,
            base_url: req.base_url,
            api_keys,
            enabled: req.enabled,
            settings: None,
        };

        let raw = self.backend.create_provider(&create_req).await?;
        Ok(mask_provider_summary(raw))
    }

    /// Update an AI provider.
    pub async fn update_provider(
        &self,
        id_or_name: &str,
        req: AiProviderUpdateRequest,
    ) -> Result<AiProviderSummary, ManagerError> {
        let mut api_keys = req.api_keys;
        if let Some(key) = req.api_key {
            if !key.trim().is_empty() {
                api_keys = Some(vec![key]);
            }
        }

        let update_req = UpdateAiProviderRequest {
            display_name: req.display_name,
            base_url: req.base_url,
            api_keys,
            enabled: req.enabled,
        };

        let raw = self
            .backend
            .update_provider(id_or_name, &update_req)
            .await?;
        Ok(mask_provider_summary(raw))
    }

    /// Delete an AI provider.
    pub async fn delete_provider(&self, id_or_name: &str) -> Result<(), ManagerError> {
        self.backend.delete_provider(id_or_name).await
    }

    // ── AI Model Management ────────────────────────────────────────────────

    /// List all configured Chat Models.
    pub async fn list_models(&self) -> Result<Vec<AiModelSummary>, ManagerError> {
        let raw = self.backend.list_models().await?;
        Ok(raw
            .into_iter()
            .map(|m| AiModelSummary {
                id: m.id,
                ai_provider_id: m.ai_provider_id,
                model: m.model,
                display_name: m.display_name,
                enabled: m.enabled,
                is_default: m.is_default,
                context_limit: m.context_limit,
                compression_threshold: m.compression_threshold,
                created_at: m.created_at,
                updated_at: m.updated_at,
            })
            .collect())
    }

    /// Register a new Chat Model.
    pub async fn create_model(
        &self,
        req: AiModelCreateRequest,
    ) -> Result<AiModelSummary, ManagerError> {
        if req.model.trim().is_empty() {
            return Err(ManagerError::InvalidRequest(
                "model identifier must not be empty".to_string(),
            ));
        }
        if req.ai_provider_id.trim().is_empty() {
            return Err(ManagerError::InvalidRequest(
                "ai_provider_id must not be empty".to_string(),
            ));
        }

        let create_req = CreateChatModelRequest {
            ai_provider_id: req.ai_provider_id,
            model: req.model,
            display_name: req.display_name,
            context_limit: req.context_limit,
            compression_threshold: req.compression_threshold,
            is_default: req.is_default,
            enabled: req.enabled,
        };

        let m = self.backend.create_model(&create_req).await?;
        Ok(AiModelSummary {
            id: m.id,
            ai_provider_id: m.ai_provider_id,
            model: m.model,
            display_name: m.display_name,
            enabled: m.enabled,
            is_default: m.is_default,
            context_limit: m.context_limit,
            compression_threshold: m.compression_threshold,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
    }

    /// Update an existing Chat Model.
    pub async fn update_model(
        &self,
        model_id: &str,
        req: AiModelUpdateRequest,
    ) -> Result<AiModelSummary, ManagerError> {
        let update_req = UpdateChatModelRequest {
            display_name: req.display_name,
            context_limit: req.context_limit,
            compression_threshold: req.compression_threshold,
            is_default: req.is_default,
            enabled: req.enabled,
        };

        let m = self.backend.update_model(model_id, &update_req).await?;
        Ok(AiModelSummary {
            id: m.id,
            ai_provider_id: m.ai_provider_id,
            model: m.model,
            display_name: m.display_name,
            enabled: m.enabled,
            is_default: m.is_default,
            context_limit: m.context_limit,
            compression_threshold: m.compression_threshold,
            created_at: m.created_at,
            updated_at: m.updated_at,
        })
    }

    /// Delete a Chat Model.
    pub async fn delete_model(&self, model_id: &str) -> Result<(), ManagerError> {
        self.backend.delete_model(model_id).await
    }

    // ── OpenFlows Model Assignment Policy ──────────────────────────────────

    /// Get global model assignment policy.
    pub async fn get_global_model_policy(&self) -> Result<ModelPolicy, ManagerError> {
        self.store
            .ping()
            .await
            .map_err(|e| ManagerError::Store(format!("Store unavailable: {e}")))?;
        let policy: Option<ModelPolicy> = self.store.get_typed(GLOBAL_MODEL_POLICY_KEY).await;
        Ok(policy.unwrap_or_default())
    }

    /// Set global model assignment policy.
    pub async fn set_global_model_policy(&self, policy: ModelPolicy) -> Result<(), ManagerError> {
        self.store
            .set_typed(GLOBAL_MODEL_POLICY_KEY, &policy)
            .await
            .map_err(|e| ManagerError::Store(e.to_string()))?;
        Ok(())
    }

    /// Get tenant-scoped model assignment policy.
    pub async fn get_tenant_model_policy(&self, tenant: &str) -> Result<ModelPolicy, ManagerError> {
        validate_tenant_name(tenant)?;
        let t_store = self.store.for_tenant(tenant);
        t_store
            .ping()
            .await
            .map_err(|e| ManagerError::Store(format!("Store unavailable: {e}")))?;
        let policy: Option<ModelPolicy> = t_store.get_typed("ai:model_policy").await;
        Ok(policy.unwrap_or_default())
    }

    /// Set tenant-scoped model assignment policy.
    pub async fn set_tenant_model_policy(
        &self,
        tenant: &str,
        policy: ModelPolicy,
    ) -> Result<(), ManagerError> {
        validate_tenant_name(tenant)?;
        let t_store = self.store.for_tenant(tenant);
        t_store
            .set_typed("ai:model_policy", &policy)
            .await
            .map_err(|e| ManagerError::Store(e.to_string()))?;
        Ok(())
    }

    /// Resolve effective model for an agent role (e.g. forge, sentinel, vessel, lore, nexus).
    /// Precedence:
    /// 1. Tenant role override
    /// 2. Tenant default_model
    /// 3. Global role override
    /// 4. Global default_model
    /// 5. Coder deployment default model
    pub async fn resolve_model_for_role(
        &self,
        role: &str,
        tenant: Option<&str>,
    ) -> Result<ResolvedModel, ManagerError> {
        // 1 & 2: Tenant policy
        if let Some(t) = tenant {
            let t_policy = self.get_tenant_model_policy(t).await?;
            if let Some(m) = t_policy.roles.get(role) {
                if !m.trim().is_empty() {
                    return Ok(ResolvedModel {
                        role: role.to_string(),
                        model: m.clone(),
                        source: ModelResolutionSource::RolePolicy,
                    });
                }
            }
            if let Some(ref d) = t_policy.default_model {
                if !d.trim().is_empty() {
                    return Ok(ResolvedModel {
                        role: role.to_string(),
                        model: d.clone(),
                        source: ModelResolutionSource::DefaultPolicy,
                    });
                }
            }
        }

        // 3 & 4: Global policy
        let g_policy = self.get_global_model_policy().await?;
        if let Some(m) = g_policy.roles.get(role) {
            if !m.trim().is_empty() {
                return Ok(ResolvedModel {
                    role: role.to_string(),
                    model: m.clone(),
                    source: ModelResolutionSource::RolePolicy,
                });
            }
        }
        if let Some(ref d) = g_policy.default_model {
            if !d.trim().is_empty() {
                return Ok(ResolvedModel {
                    role: role.to_string(),
                    model: d.clone(),
                    source: ModelResolutionSource::DefaultPolicy,
                });
            }
        }

        // 5: Coder default model
        if let Ok(models) = self.list_models().await {
            if let Some(default_m) = models.iter().find(|m| m.is_default && m.enabled) {
                return Ok(ResolvedModel {
                    role: role.to_string(),
                    model: default_m.model.clone(),
                    source: ModelResolutionSource::CoderDefault,
                });
            }
            if let Some(first_m) = models.iter().find(|m| m.enabled) {
                return Ok(ResolvedModel {
                    role: role.to_string(),
                    model: first_m.model.clone(),
                    source: ModelResolutionSource::CoderDefault,
                });
            }
        }

        // Fallback default
        Ok(ResolvedModel {
            role: role.to_string(),
            model: "default".to_string(),
            source: ModelResolutionSource::CoderDefault,
        })
    }
}

/// Convert a CoderAiProvider to an AiProviderSummary, strictly excluding secrets.
fn mask_provider_summary(p: CoderAiProvider) -> AiProviderSummary {
    AiProviderSummary {
        id: p.id,
        provider_type: p.provider_type,
        name: p.name,
        display_name: p.display_name,
        base_url: p.base_url,
        enabled: p.enabled,
        has_api_key: !p.api_keys.is_empty(),
        created_at: p.created_at,
        updated_at: p.updated_at,
    }
}
