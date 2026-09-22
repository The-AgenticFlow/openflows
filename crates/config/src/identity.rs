// crates/config/src/identity.rs
//
// IdentityManager — centralized identity management for all agents.
//
// Provides a unified interface for agent identity resolution:
// - Model backend configuration
// - GitHub token resolution with per-agent env var support
// - Routing keys for proxy integration
// - Agent role classification
//
// Design:
// - Wraps Registry for loading from registry.json
// - Caches resolved tokens to avoid repeated env var lookups
// - Arc-compatible for sharing across threads/agents
// - Supports both base roles (forge) and instance slots (forge-1, forge-2)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::Path;
use std::str::FromStr;
use std::sync::{Arc, RwLock};

use crate::registry::{Registry, RegistryEntry};

/// Agent role classification — distinguishes agent types.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    Nexus,
    Forge,
    Sentinel,
    Vessel,
    Lore,
}

impl AgentRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentRole::Nexus => "nexus",
            AgentRole::Forge => "forge",
            AgentRole::Sentinel => "sentinel",
            AgentRole::Vessel => "vessel",
            AgentRole::Lore => "lore",
        }
    }

    pub fn all() -> &'static [AgentRole] {
        &[
            AgentRole::Nexus,
            AgentRole::Forge,
            AgentRole::Sentinel,
            AgentRole::Vessel,
            AgentRole::Lore,
        ]
    }
}

impl FromStr for AgentRole {
    type Err = String;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nexus" => Ok(AgentRole::Nexus),
            "forge" => Ok(AgentRole::Forge),
            "sentinel" => Ok(AgentRole::Sentinel),
            "vessel" => Ok(AgentRole::Vessel),
            "lore" => Ok(AgentRole::Lore),
            _ => Err(format!("Unknown agent role: {}", s)),
        }
    }
}

impl std::fmt::Display for AgentRole {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.as_str())
    }
}

/// Complete identity for a single agent instance.
/// This is the resolved form of RegistryEntry with tokens already fetched.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AgentIdentity {
    pub id: String,
    pub role: AgentRole,
    pub instance_num: Option<u32>,
    pub model_backend: Option<String>,
    pub github_token: String,
    pub routing_key: Option<String>,
    pub cli: String,
    pub active: bool,
}

impl AgentIdentity {
    pub fn is_forge_instance(&self) -> bool {
        self.role == AgentRole::Forge && self.instance_num.is_some()
    }

    pub fn slot_name(&self) -> &str {
        &self.id
    }

    pub fn is_active(&self) -> bool {
        self.active
    }
}

/// Internal cache for resolved tokens.
#[derive(Debug, Default)]
struct TokenCache {
    tokens: HashMap<String, String>,
}

/// Centralized identity management for all agents.
///
/// Thread-safe via Arc<RwLock<...>> for shared access across agents.
///
/// # Example
///
/// ```ignore
/// use config::identity::{IdentityManager, AgentRole};
///
/// let manager = IdentityManager::load("orchestration/agent/registry.json")?;
///
/// // Get identity for a specific agent
/// let nexus_identity = manager.get_identity("nexus")?;
/// println!("Nexus model: {:?}", nexus_identity.model_backend);
///
/// // Get all forge instance identities
/// let forge_identities = manager.get_identities_for_role(AgentRole::Forge)?;
///
/// // Get GitHub token for any agent
/// let token = manager.resolve_github_token("forge-1")?;
/// ```
#[derive(Debug, Clone)]
pub struct IdentityManager {
    registry: Arc<RwLock<Registry>>,
    token_cache: Arc<RwLock<TokenCache>>,
}

impl IdentityManager {
    /// Load from registry.json at the given path.
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let registry = Registry::load(path)?;
        Ok(Self {
            registry: Arc::new(RwLock::new(registry)),
            token_cache: Arc::new(RwLock::new(TokenCache::default())),
        })
    }

    /// Create from an existing Registry (for testing or custom loading).
    pub fn from_registry(registry: Registry) -> Self {
        Self {
            registry: Arc::new(RwLock::new(registry)),
            token_cache: Arc::new(RwLock::new(TokenCache::default())),
        }
    }

    /// Reload registry from disk (for hot-reloading configuration).
    pub fn reload(&self, path: impl AsRef<Path>) -> Result<()> {
        let registry = Registry::load(path)?;
        {
            let mut reg = self
                .registry
                .write()
                .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
            *reg = registry;
        }
        {
            let mut cache = self
                .token_cache
                .write()
                .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
            cache.tokens.clear();
        }
        Ok(())
    }

    /// Get the underlying Registry (read-only access).
    pub fn registry(&self) -> Result<Registry> {
        let reg = self
            .registry
            .read()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        Ok(reg.clone())
    }

    /// Get identity for a specific agent by ID (e.g., "nexus", "forge-1", "sentinel").
    ///
    /// For forge instances, this looks up the base "forge" entry and creates
    /// an identity with the instance number appended.
    pub fn get_identity(&self, agent_id: &str) -> Result<AgentIdentity> {
        let reg = self
            .registry
            .read()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let (entry, role, instance_num) = Self::resolve_entry(&reg, agent_id)?;
        let github_token = self.resolve_token_for_entry(&entry)?;
        drop(reg);

        Ok(AgentIdentity {
            id: agent_id.to_string(),
            role,
            instance_num,
            model_backend: entry.model_backend.clone(),
            github_token,
            routing_key: entry.routing_key.clone(),
            cli: entry.cli.clone(),
            active: entry.active,
        })
    }

    /// Resolve the RegistryEntry for a given agent_id.
    /// Handles both base roles ("forge") and instance slots ("forge-1").
    fn resolve_entry(
        registry: &Registry,
        agent_id: &str,
    ) -> Result<(RegistryEntry, AgentRole, Option<u32>)> {
        if let Ok(role) = AgentRole::from_str(agent_id) {
            let entry = registry.get(agent_id).cloned().ok_or_else(|| {
                anyhow::anyhow!("Agent '{}' not found in registry or inactive", agent_id)
            })?;
            return Ok((entry, role, None));
        }

        if agent_id.starts_with("forge-") {
            let instance_str = agent_id.strip_prefix("forge-").unwrap();
            let instance_num: u32 = instance_str
                .parse()
                .with_context(|| format!("Invalid forge instance number: {}", instance_str))?;

            let entry = registry.get("forge").cloned().ok_or_else(|| {
                anyhow::anyhow!("Base agent 'forge' not found in registry or inactive")
            })?;

            // Use effective_instances() so v2 registries that declare only
            // `max_instances` (and omit the deprecated v1 `instances`, which
            // defaults to 0) are honored here. Without this, forge-1 would always
            // fail validation against instances==0 ("exceeds configured instances (0)")
            // even when max_instances is 2, blocking all forge token resolution
            // and therefore all forge work assignment.
            let effective = entry.effective_instances();
            if instance_num > effective {
                return Err(anyhow::anyhow!(
                    "Forge instance {} exceeds configured instances ({})",
                    instance_num,
                    effective
                ));
            }

            return Ok((entry, AgentRole::Forge, Some(instance_num)));
        }

        for role in AgentRole::all() {
            let prefix = format!("{}-", role.as_str());
            if agent_id.starts_with(&prefix) {
                let instance_str = agent_id.strip_prefix(&prefix).unwrap();
                let instance_num: u32 = instance_str.parse().with_context(|| {
                    format!(
                        "Invalid instance number for {}: {}",
                        role.as_str(),
                        instance_str
                    )
                })?;

                let entry = registry.get(role.as_str()).cloned().ok_or_else(|| {
                    anyhow::anyhow!(
                        "Base agent '{}' not found in registry or inactive",
                        role.as_str()
                    )
                })?;

                // Validate instance number against effective_instances() (honors
                // max_instances for v2 registries whose deprecated `instances`
                // field is absent/0). Mirrors the forge branch.
                let effective = entry.effective_instances();
                if instance_num > effective {
                    return Err(anyhow::anyhow!(
                        "{} instance {} exceeds configured instances ({})",
                        role.as_str(),
                        instance_num,
                        effective
                    ));
                }

                return Ok((entry, *role, Some(instance_num)));
            }
        }

        Err(anyhow::anyhow!("Unknown agent ID format: {}", agent_id))
    }

    /// Resolve GitHub token for an agent (with caching).
    pub fn resolve_github_token(&self, agent_id: &str) -> Result<String> {
        let reg = self
            .registry
            .read()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let (entry, _, _) = Self::resolve_entry(&reg, agent_id)?;
        drop(reg);
        self.resolve_token_for_entry(&entry)
    }

    fn resolve_token_for_entry(&self, entry: &RegistryEntry) -> Result<String> {
        {
            let cache = self
                .token_cache
                .read()
                .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
            if let Some(cached) = cache.tokens.get(&entry.id) {
                return Ok(cached.clone());
            }
        }

        let token = match &entry.github_token_env {
            Some(env_var) => std::env::var(env_var)
                .with_context(|| format!("{} not set for agent {}", env_var, entry.id))?,
            None => std::env::var("GITHUB_TOKEN")
                .context("GITHUB_TOKEN not set (fallback for agent without github_token_env)")?,
        };

        {
            let mut cache = self
                .token_cache
                .write()
                .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
            cache.tokens.insert(entry.id.clone(), token.clone());
        }

        Ok(token)
    }

    /// Get all identities for a specific role.
    /// For Forge, returns all instances (forge-1, forge-2, ...).
    /// For other roles, returns a single identity if active.
    pub fn get_identities_for_role(&self, role: AgentRole) -> Result<Vec<AgentIdentity>> {
        let reg = self
            .registry
            .read()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;

        let entry = match reg.get(role.as_str()) {
            Some(e) => e.clone(),
            None => return Ok(Vec::new()),
        };
        drop(reg);

        if role == AgentRole::Forge {
            let mut identities = Vec::with_capacity(entry.instances as usize);
            for i in 1..=entry.instances {
                let id = format!("forge-{}", i);
                let github_token = self.resolve_token_for_entry(&entry)?;
                identities.push(AgentIdentity {
                    id,
                    role: AgentRole::Forge,
                    instance_num: Some(i),
                    model_backend: entry.model_backend.clone(),
                    github_token,
                    routing_key: entry.routing_key.clone(),
                    cli: entry.cli.clone(),
                    active: entry.active,
                });
            }
            return Ok(identities);
        }

        let github_token = self.resolve_token_for_entry(&entry)?;
        Ok(vec![AgentIdentity {
            id: role.as_str().to_string(),
            role,
            instance_num: None,
            model_backend: entry.model_backend.clone(),
            github_token,
            routing_key: entry.routing_key.clone(),
            cli: entry.cli.clone(),
            active: entry.active,
        }])
    }

    /// Get all active agent identities (including all forge instances).
    pub fn all_identities(&self) -> Result<Vec<AgentIdentity>> {
        let mut identities = Vec::new();
        for role in AgentRole::all() {
            identities.extend(self.get_identities_for_role(*role)?);
        }
        Ok(identities)
    }

    /// Get all worker slot names (for SharedStore initialization).
    pub fn all_worker_slots(&self) -> Result<Vec<String>> {
        let reg = self
            .registry
            .read()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        Ok(reg.all_worker_slots())
    }

    /// Get forge worker slot names only.
    pub fn forge_slots(&self) -> Result<Vec<String>> {
        let reg = self
            .registry
            .read()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        Ok(reg.forge_slots())
    }

    /// Get the model_backend for a specific agent.
    pub fn get_model_backend(&self, agent_id: &str) -> Result<Option<String>> {
        let reg = self
            .registry
            .read()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let (entry, _, _) = Self::resolve_entry(&reg, agent_id)?;
        Ok(entry.model_backend.clone())
    }

    /// Get the routing_key for a specific agent.
    pub fn get_routing_key(&self, agent_id: &str) -> Result<Option<String>> {
        let reg = self
            .registry
            .read()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        let (entry, _, _) = Self::resolve_entry(&reg, agent_id)?;
        Ok(entry.routing_key.clone())
    }

    /// Clear the token cache (for testing or token rotation).
    pub fn clear_cache(&self) -> Result<()> {
        let mut cache = self
            .token_cache
            .write()
            .map_err(|e| anyhow::anyhow!("Lock poisoned: {}", e))?;
        cache.tokens.clear();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    fn sample_registry_json() -> &'static str {
        r#"{
          "team": [
            { "id": "nexus",    "cli": "claude", "active": true,  "instances": 1, "model_backend": "accounts/fireworks/models/kimi-k2p6", "routing_key": "nexus-key" },
            { "id": "forge",    "cli": "claude", "active": true,  "instances": 2, "model_backend": "accounts/fireworks/models/kimi-k2p6", "routing_key": "forge-key" },
            { "id": "sentinel", "cli": "claude", "active": true,  "instances": 1, "model_backend": "accounts/fireworks/models/kimi-k2p6", "routing_key": "sentinel-key" },
            { "id": "vessel",   "cli": "claude", "active": true,  "instances": 1, "model_backend": "accounts/fireworks/models/kimi-k2p6", "routing_key": "vessel-key" },
            { "id": "lore",     "cli": "claude", "active": true,  "instances": 1, "model_backend": "accounts/fireworks/models/kimi-k2p6", "routing_key": "lore-key" }
          ]
        }"#
    }

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    fn setup_test_token() {
        std::env::set_var("GITHUB_TOKEN", "test-token");
    }

    #[test]
    fn test_agent_role_from_str() {
        assert_eq!(AgentRole::from_str("nexus"), Ok(AgentRole::Nexus));
        assert_eq!(AgentRole::from_str("forge"), Ok(AgentRole::Forge));
        assert!(AgentRole::from_str("unknown").is_err());
    }

    #[test]
    fn test_agent_role_display() {
        assert_eq!(format!("{}", AgentRole::Nexus), "nexus");
        assert_eq!(format!("{}", AgentRole::Forge), "forge");
    }

    #[test]
    fn test_load_identity_manager() {
        setup_test_token();
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();
        let identities = manager.all_identities().unwrap();
        assert_eq!(identities.len(), 6); // nexus + 2 forge + sentinel + vessel + lore
    }

    #[test]
    fn test_get_identity_forge_instance() {
        setup_test_token();
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let forge_1 = manager.get_identity("forge-1").unwrap();
        assert_eq!(forge_1.role, AgentRole::Forge);
        assert_eq!(forge_1.instance_num, Some(1));
        assert!(forge_1.is_forge_instance());

        let forge_2 = manager.get_identity("forge-2").unwrap();
        assert_eq!(forge_2.instance_num, Some(2));
    }

    #[test]
    fn test_get_identity_nexus() {
        setup_test_token();
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let nexus = manager.get_identity("nexus").unwrap();
        assert_eq!(nexus.role, AgentRole::Nexus);
        assert_eq!(nexus.instance_num, None);
        assert_eq!(
            nexus.model_backend,
            Some("accounts/fireworks/models/kimi-k2p6".to_string())
        );
    }

    #[test]
    fn test_get_identities_for_role_forge() {
        setup_test_token();
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let forge_identities = manager.get_identities_for_role(AgentRole::Forge).unwrap();
        assert_eq!(forge_identities.len(), 2);
        assert_eq!(forge_identities[0].id, "forge-1");
        assert_eq!(forge_identities[1].id, "forge-2");
    }

    #[test]
    fn test_get_identities_for_role_nexus() {
        setup_test_token();
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let nexus_identities = manager.get_identities_for_role(AgentRole::Nexus).unwrap();
        assert_eq!(nexus_identities.len(), 1);
        assert_eq!(nexus_identities[0].id, "nexus");
    }

    #[test]
    fn test_forge_slots() {
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let slots = manager.forge_slots().unwrap();
        assert_eq!(slots, vec!["forge-1", "forge-2"]);
    }

    #[test]
    fn test_all_worker_slots() {
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let slots = manager.all_worker_slots().unwrap();
        assert!(slots.contains(&"nexus".to_string()));
        assert!(slots.contains(&"forge-1".to_string()));
        assert!(slots.contains(&"forge-2".to_string()));
        assert!(slots.contains(&"sentinel".to_string()));
        assert!(slots.contains(&"vessel".to_string()));
        assert!(slots.contains(&"lore".to_string()));
    }

    #[test]
    fn test_invalid_forge_instance() {
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let result = manager.get_identity("forge-3");
        assert!(result.is_err());
        assert!(result
            .unwrap_err()
            .to_string()
            .contains("exceeds configured instances"));
    }

    #[test]
    fn test_unknown_agent() {
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let result = manager.get_identity("unknown");
        assert!(result.is_err());
    }

    #[test]
    fn test_get_model_backend() {
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let model = manager.get_model_backend("nexus").unwrap();
        assert_eq!(
            model,
            Some("accounts/fireworks/models/kimi-k2p6".to_string())
        );
    }

    #[test]
    fn test_get_routing_key() {
        let f = write_temp(sample_registry_json());
        let manager = IdentityManager::load(f.path()).unwrap();

        let key = manager.get_routing_key("forge-1").unwrap();
        assert_eq!(key, Some("forge-key".to_string()));
    }
}
