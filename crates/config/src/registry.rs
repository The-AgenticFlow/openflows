// crates/config/src/registry.rs
//
// Reads orchestration/agent/registry.json — single source of truth for team membership.
// NEXUS reloads this on every poll cycle for zero-downtime team changes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A single agent entry from registry.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistryEntry {
    pub id: String,
    pub cli: String, // "claude" | "gemini" | "codex"
    pub active: bool,
    pub instances: u32, // registry.json is sole source — .agent.md has no instances field
    #[serde(default)]
    pub model_backend: Option<String>, // e.g. "anthropic/claude-sonnet-4-5", "gemini/gemini-2.5-pro"
    #[serde(default)]
    pub routing_key: Option<String>, // LiteLLM proxy routing key, e.g. "forge-key"
    #[serde(default)]
    pub github_token_env: Option<String>, // Per-agent GitHub token env var, e.g. "AGENT_NEXUS_GITHUB_TOKEN"
}

/// The full registry — a thin wrapper around the team list.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Registry {
    pub team: Vec<RegistryEntry>,
}

impl Registry {
    /// Load from a path (typically `orchestration/agent/registry.json`).
    pub fn load(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read registry at {}", path.display()))?;
        let registry: Registry = serde_json::from_str(&content)
            .with_context(|| format!("Failed to parse registry at {}", path.display()))?;
        Ok(registry)
    }

    /// Active agents only.
    pub fn active_agents(&self) -> impl Iterator<Item = &RegistryEntry> {
        self.team.iter().filter(|e| e.active)
    }

    /// Look up a specific agent by id. Returns None if not found or inactive.
    pub fn get(&self, id: &str) -> Option<&RegistryEntry> {
        self.team.iter().find(|e| e.id == id && e.active)
    }

    /// Normalize agent ID by stripping instance suffix (e.g., "forge-1" -> "forge").
    /// Returns the base ID if no suffix, or if the ID is exactly in the registry.
    fn normalize_agent_id<'a>(&self, agent_id: &'a str) -> &'a str {
        if self.get(agent_id).is_some() {
            return agent_id;
        }
        if let Some(pos) = agent_id.rfind('-') {
            let base = &agent_id[..pos];
            if self.get(base).is_some() {
                return base;
            }
        }
        agent_id
    }

    /// Total active instance count across all agents.
    pub fn total_instances(&self) -> u32 {
        self.active_agents().map(|e| e.instances).sum()
    }

    /// FORGE worker slot names: ["forge-1", "forge-2", ...]
    pub fn forge_slots(&self) -> Vec<String> {
        match self.get("forge") {
            None => vec![],
            Some(entry) => (1..=entry.instances)
                .map(|i| format!("forge-{}", i))
                .collect(),
        }
    }

    /// All worker slot names including non-forge agents like "lore".
    /// Returns slots for all active agents with instances > 0.
    pub fn all_worker_slots(&self) -> Vec<String> {
        let mut slots = Vec::new();
        for entry in self.active_agents() {
            if entry.instances > 0 {
                if entry.id == "forge" {
                    for i in 1..=entry.instances {
                        slots.push(format!("forge-{}", i));
                    }
                } else if entry.instances == 1 {
                    slots.push(entry.id.clone());
                } else {
                    for i in 1..=entry.instances {
                        slots.push(format!("{}-{}", entry.id, i));
                    }
                }
            }
        }
        slots
    }

    /// Resolve GitHub token for a given agent.
    /// If the agent has `github_token_env` set, tries that env var first.
    /// Falls back to `GITHUB_PERSONAL_ACCESS_TOKEN` if the agent-specific var is not set.
    /// Handles instance IDs (e.g., "forge-1") by stripping suffix to find base agent.
    pub fn resolve_github_token(&self, agent_id: &str) -> Result<String> {
        let base_id = self.normalize_agent_id(agent_id);
        let token = match self.get(base_id) {
            Some(entry) => match &entry.github_token_env {
                Some(env_var) => {
                    // Try agent-specific token first, fall back to generic token
                    std::env::var(env_var).or_else(|_| {
                        std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN")
                            .with_context(|| format!(
                                "Neither {} nor GITHUB_PERSONAL_ACCESS_TOKEN is set for agent {}",
                                env_var, agent_id
                            ))
                    })?
                }
                None => std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN")
                    .context("GITHUB_PERSONAL_ACCESS_TOKEN not set (fallback for agent without github_token_env)")?,
            },
            None => std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN")
                .context("GITHUB_PERSONAL_ACCESS_TOKEN not set (agent not found in registry)")?,
        };
        Ok(token)
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
            { "id": "nexus",    "cli": "claude", "active": true,  "instances": 1, "model_backend": "anthropic/claude-sonnet-4-5", "routing_key": "nexus-key" },
            { "id": "forge",    "cli": "claude", "active": true,  "instances": 2, "model_backend": "anthropic/claude-sonnet-4-5", "routing_key": "forge-key" },
            { "id": "sentinel", "cli": "claude", "active": true,  "instances": 1, "model_backend": "gemini/gemini-2.5-pro",      "routing_key": "sentinel-key" },
            { "id": "vessel",   "cli": "claude", "active": true,  "instances": 1, "model_backend": "groq/llama-3.3-70b-versatile", "routing_key": "vessel-key" },
            { "id": "lore",     "cli": "claude", "active": false, "instances": 1, "model_backend": "openai/gpt-4o-mini",       "routing_key": "lore-key" }
          ]
        }"#
    }

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    #[test]
    fn test_load_registry() {
        let f = write_temp(sample_registry_json());
        let reg = Registry::load(f.path()).unwrap();
        assert_eq!(reg.team.len(), 5);
    }

    #[test]
    fn test_active_agents_excludes_inactive() {
        let f = write_temp(sample_registry_json());
        let reg = Registry::load(f.path()).unwrap();
        let active: Vec<_> = reg.active_agents().collect();
        assert_eq!(active.len(), 4); // lore is inactive
        assert!(active.iter().all(|e| e.active));
    }

    #[test]
    fn test_forge_slots() {
        let f = write_temp(sample_registry_json());
        let reg = Registry::load(f.path()).unwrap();
        assert_eq!(reg.forge_slots(), vec!["forge-1", "forge-2"]);
    }

    #[test]
    fn test_get_inactive_returns_none() {
        let f = write_temp(sample_registry_json());
        let reg = Registry::load(f.path()).unwrap();
        assert!(reg.get("lore").is_none());
    }

    #[test]
    fn test_get_active_returns_some() {
        let f = write_temp(sample_registry_json());
        let reg = Registry::load(f.path()).unwrap();
        let nexus = reg.get("nexus").unwrap();
        assert_eq!(nexus.instances, 1);
    }

    #[test]
    fn test_normalize_agent_id() {
        let f = write_temp(sample_registry_json());
        let reg = Registry::load(f.path()).unwrap();
        assert_eq!(reg.normalize_agent_id("forge"), "forge");
        assert_eq!(reg.normalize_agent_id("forge-1"), "forge");
        assert_eq!(reg.normalize_agent_id("forge-2"), "forge");
        assert_eq!(reg.normalize_agent_id("nexus"), "nexus");
        assert_eq!(reg.normalize_agent_id("unknown"), "unknown");
    }
}
