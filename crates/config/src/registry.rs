// crates/config/src/registry.rs
//
// Reads orchestration/agent/registry.json — single source of truth for team membership.
// NEXUS reloads this on every poll cycle for zero-downtime team changes.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// CLI backend type for agent execution.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Default, Hash)]
#[serde(rename_all = "lowercase")]
pub enum CliBackend {
    /// Claude Code CLI (default)
    #[default]
    Claude,
    /// OpenAI Codex CLI
    Codex,
}

impl std::str::FromStr for CliBackend {
    type Err = std::convert::Infallible;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Ok(match s.to_lowercase().as_str() {
            "codex" => CliBackend::Codex,
            "claude" => CliBackend::Claude,
            _ => CliBackend::Claude, // Default fallback
        })
    }
}

impl CliBackend {
    /// Parse from string, with fallback to default.
    pub fn parse(s: &str) -> Self {
        s.parse().unwrap_or(CliBackend::Claude)
    }

    /// Convert to string for display.
    pub fn as_str(&self) -> &'static str {
        match self {
            CliBackend::Claude => "claude",
            CliBackend::Codex => "codex",
        }
    }

    /// Get the CLI binary name/path.
    pub fn binary_name(&self) -> &'static str {
        match self {
            CliBackend::Claude => "claude",
            CliBackend::Codex => "codex",
        }
    }

    /// Get the environment variable for CLI path override.
    pub fn path_env_var(&self) -> &'static str {
        match self {
            CliBackend::Claude => "CLAUDE_PATH",
            CliBackend::Codex => "CODEX_PATH",
        }
    }
}

/// A single agent entry from registry.json.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct RegistryEntry {
    pub id: String,
    #[serde(default)]
    pub cli: String, // "claude" | "codex" - defaults to registry's default_cli or "claude"
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
    /// Default CLI backend for agents without explicit cli field
    #[serde(default = "default_cli")]
    pub default_cli: String,
    pub team: Vec<RegistryEntry>,
}

fn default_cli() -> String {
    "claude".to_string()
}

/// Environment variable name for overriding the default CLI backend.
pub const DEFAULT_CLI_ENV_VAR: &str = "DEFAULT_CLI";

impl RegistryEntry {
    /// Get the CLI backend for this agent, respecting priority:
    /// 1. Agent-specific `cli` field (highest priority)
    /// 2. Provided default (from env var or registry default_cli)
    /// 3. Hardcoded "claude" fallback
    pub fn cli_backend(&self, default: &str) -> CliBackend {
        let cli = if self.cli.is_empty() {
            default
        } else {
            &self.cli
        };
        CliBackend::parse(cli)
    }
}

impl Registry {
    /// Get the effective default CLI backend, respecting priority:
    /// 1. DEFAULT_CLI environment variable (highest priority)
    /// 2. registry.json default_cli field
    /// 3. Hardcoded "claude" fallback
    pub fn effective_default_cli(&self) -> CliBackend {
        // Check environment variable first
        if let Ok(env_cli) = std::env::var(DEFAULT_CLI_ENV_VAR) {
            if !env_cli.is_empty() {
                return CliBackend::parse(&env_cli);
            }
        }
        // Fall back to registry default_cli
        CliBackend::parse(&self.default_cli)
    }

    /// Resolve CLI backend for a specific agent, respecting priority:
    /// 1. Agent-specific `cli` field (highest priority)
    /// 2. DEFAULT_CLI environment variable
    /// 3. registry.json default_cli field
    /// 4. Hardcoded "claude" fallback
    pub fn resolve_cli_backend(&self, agent_id: &str) -> CliBackend {
        let base_id = self.normalize_agent_id(agent_id);
        match self.get(base_id) {
            Some(entry) => {
                // If agent has explicit cli field, use it
                if !entry.cli.is_empty() {
                    return CliBackend::parse(&entry.cli);
                }
                // Otherwise use effective default (env var > registry default)
                self.effective_default_cli()
            }
            None => {
                // Agent not found, use effective default
                self.effective_default_cli()
            }
        }
    }
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
    /// If the agent has `github_token_env` set, reads from that env var.
    /// Falls back to `GITHUB_PERSONAL_ACCESS_TOKEN` for backward compatibility.
    /// Handles instance IDs (e.g., "forge-1") by stripping suffix to find base agent.
    pub fn resolve_github_token(&self, agent_id: &str) -> Result<String> {
        let base_id = self.normalize_agent_id(agent_id);
        let token = match self.get(base_id) {
            Some(entry) => match &entry.github_token_env {
                Some(env_var) => std::env::var(env_var)
                    .with_context(|| format!("{} not set for agent {}", env_var, agent_id))?,
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
          "default_cli": "claude",
          "team": [
            { "id": "nexus",    "cli": "claude", "active": true,  "instances": 1, "model_backend": "anthropic/claude-sonnet-4-5", "routing_key": "nexus-key" },
            { "id": "forge",    "cli": "claude", "active": true,  "instances": 2, "model_backend": "anthropic/claude-sonnet-4-5", "routing_key": "forge-key" },
            { "id": "sentinel", "cli": "claude", "active": true,  "instances": 1, "model_backend": "gemini/gemini-2.5-pro",      "routing_key": "sentinel-key" },
            { "id": "vessel",   "cli": "claude", "active": true,  "instances": 1, "model_backend": "groq/llama-3.3-70b-versatile", "routing_key": "vessel-key" },
            { "id": "lore",     "cli": "claude", "active": false, "instances": 1, "model_backend": "openai/gpt-4o-mini",       "routing_key": "lore-key" }
          ]
        }"#
    }

    fn sample_registry_with_codex() -> &'static str {
        r#"{
          "default_cli": "claude",
          "team": [
            { "id": "nexus",    "cli": "codex",  "active": true,  "instances": 1, "model_backend": "openai/gpt-4o", "routing_key": "nexus-key" },
            { "id": "forge",    "cli": "codex",  "active": true,  "instances": 2, "model_backend": "openai/gpt-4o", "routing_key": "forge-key" },
            { "id": "sentinel", "cli": "claude", "active": true,  "instances": 1, "model_backend": "anthropic/claude-sonnet-4-5", "routing_key": "sentinel-key" },
            { "id": "vessel",   "cli": "claude", "active": true,  "instances": 1, "model_backend": "anthropic/claude-sonnet-4-5", "routing_key": "vessel-key" },
            { "id": "lore",     "cli": "codex",  "active": true,  "instances": 1, "model_backend": "openai/gpt-4o", "routing_key": "lore-key" }
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
    fn test_default_cli_backend() {
        let f = write_temp(sample_registry_json());
        let reg = Registry::load(f.path()).unwrap();
        assert_eq!(reg.default_cli, "claude");
    }

    #[test]
    fn test_cli_backend_parse() {
        assert_eq!(CliBackend::parse("claude"), CliBackend::Claude);
        assert_eq!(CliBackend::parse("CODEX"), CliBackend::Codex);
        assert_eq!(CliBackend::parse("Codex"), CliBackend::Codex);
        assert_eq!(CliBackend::parse("unknown"), CliBackend::Claude); // fallback
    }

    #[test]
    fn test_cli_backend_as_str() {
        assert_eq!(CliBackend::Claude.as_str(), "claude");
        assert_eq!(CliBackend::Codex.as_str(), "codex");
    }

    #[test]
    fn test_registry_with_mixed_cli_backends() {
        let f = write_temp(sample_registry_with_codex());
        let reg = Registry::load(f.path()).unwrap();

        // Check default_cli
        assert_eq!(reg.default_cli, "claude");

        // Check individual agents
        let nexus = reg.get("nexus").unwrap();
        assert_eq!(nexus.cli_backend(&reg.default_cli), CliBackend::Codex);

        let forge = reg.get("forge").unwrap();
        assert_eq!(forge.cli_backend(&reg.default_cli), CliBackend::Codex);

        let sentinel = reg.get("sentinel").unwrap();
        assert_eq!(sentinel.cli_backend(&reg.default_cli), CliBackend::Claude);

        let lore = reg.get("lore").unwrap();
        assert_eq!(lore.cli_backend(&reg.default_cli), CliBackend::Codex);
    }

    #[test]
    fn test_agent_cli_respects_default() {
        let json = r#"{
          "default_cli": "codex",
          "team": [
            { "id": "nexus", "cli": "", "active": true, "instances": 1 },
            { "id": "forge", "cli": "claude", "active": true, "instances": 1 }
          ]
        }"#;
        let f = write_temp(json);
        let reg = Registry::load(f.path()).unwrap();

        // nexus has empty cli, should use default
        let nexus = reg.get("nexus").unwrap();
        assert_eq!(nexus.cli_backend(&reg.default_cli), CliBackend::Codex);

        // forge has explicit claude
        let forge = reg.get("forge").unwrap();
        assert_eq!(forge.cli_backend(&reg.default_cli), CliBackend::Claude);
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

    #[test]
    fn test_effective_default_cli_without_env() {
        // When DEFAULT_CLI env var is not set, should use registry default_cli
        let f = write_temp(sample_registry_json());
        let reg = Registry::load(f.path()).unwrap();
        // Clear env var if set
        std::env::remove_var(DEFAULT_CLI_ENV_VAR);
        assert_eq!(reg.effective_default_cli(), CliBackend::Claude);
    }

    #[test]
    fn test_effective_default_cli_with_env_override() {
        // When DEFAULT_CLI env var is set, it should override registry default_cli
        let f = write_temp(sample_registry_json());
        let reg = Registry::load(f.path()).unwrap();

        // Set env var to codex
        std::env::set_var(DEFAULT_CLI_ENV_VAR, "codex");
        assert_eq!(reg.effective_default_cli(), CliBackend::Codex);

        // Clean up
        std::env::remove_var(DEFAULT_CLI_ENV_VAR);
    }

    #[test]
    fn test_resolve_cli_backend_agent_specific() {
        // Agent-specific cli field should take highest priority
        let f = write_temp(sample_registry_with_codex());
        let reg = Registry::load(f.path()).unwrap();

        // nexus has cli: "codex" in registry
        assert_eq!(reg.resolve_cli_backend("nexus"), CliBackend::Codex);

        // sentinel has cli: "claude" in registry
        assert_eq!(reg.resolve_cli_backend("sentinel"), CliBackend::Claude);
    }

    #[test]
    fn test_resolve_cli_backend_env_override() {
        // When agent has no explicit cli, DEFAULT_CLI env var should be used
        let json = r#"{
          "default_cli": "claude",
          "team": [
            { "id": "nexus", "cli": "", "active": true, "instances": 1 },
            { "id": "forge", "cli": "codex", "active": true, "instances": 1 }
          ]
        }"#;
        let f = write_temp(json);
        let reg = Registry::load(f.path()).unwrap();

        // Set env var to codex
        std::env::set_var(DEFAULT_CLI_ENV_VAR, "codex");

        // nexus has empty cli, should use env var (codex)
        assert_eq!(reg.resolve_cli_backend("nexus"), CliBackend::Codex);

        // forge has explicit cli: "codex", should still use that (highest priority)
        assert_eq!(reg.resolve_cli_backend("forge"), CliBackend::Codex);

        // Clean up
        std::env::remove_var(DEFAULT_CLI_ENV_VAR);
    }

    #[test]
    fn test_resolve_cli_backend_fallback_chain() {
        // Test the full fallback chain: agent-specific > env var > registry default > hardcoded
        let json = r#"{
          "default_cli": "claude",
          "team": [
            { "id": "agent1", "cli": "codex", "active": true, "instances": 1 },
            { "id": "agent2", "cli": "", "active": true, "instances": 1 }
          ]
        }"#;
        let f = write_temp(json);
        let reg = Registry::load(f.path()).unwrap();

        // Case 1: agent-specific cli (highest priority)
        assert_eq!(reg.resolve_cli_backend("agent1"), CliBackend::Codex);

        // Case 2: no agent-specific, no env var -> use registry default
        std::env::remove_var(DEFAULT_CLI_ENV_VAR);
        assert_eq!(reg.resolve_cli_backend("agent2"), CliBackend::Claude);

        // Case 3: no agent-specific, env var set -> use env var
        std::env::set_var(DEFAULT_CLI_ENV_VAR, "codex");
        assert_eq!(reg.resolve_cli_backend("agent2"), CliBackend::Codex);

        // Clean up
        std::env::remove_var(DEFAULT_CLI_ENV_VAR);
    }

    #[test]
    fn test_resolve_cli_backend_with_instance_id() {
        // Test that instance IDs (e.g., "forge-1") are normalized to base ID
        let f = write_temp(sample_registry_with_codex());
        let reg = Registry::load(f.path()).unwrap();

        // forge has cli: "codex" in registry
        assert_eq!(reg.resolve_cli_backend("forge-1"), CliBackend::Codex);
        assert_eq!(reg.resolve_cli_backend("forge-2"), CliBackend::Codex);
    }
}
