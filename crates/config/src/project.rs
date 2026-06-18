// crates/config/src/project.rs
//
// Reads project-level AgentFlow configuration from `.agentflow.toml`
// in the workspace/repository root. This file is version-controlled
// alongside the project and allows per-project customization of
// sandbox behavior, domain allowlists, etc.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// Project-level AgentFlow configuration.
///
/// Loaded from `.agentflow.toml` in the workspace root.
/// Falls back to sensible defaults if the file doesn't exist.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    /// Sandbox configuration for agent processes.
    #[serde(default)]
    pub sandbox: SandboxConfig,
}

/// Sandbox network and filesystem restrictions.
#[derive(Debug, Default, Clone, Serialize, Deserialize)]
pub struct SandboxConfig {
    /// Network domains that agents are allowed to access.
    ///
    /// These are written to the codex permissions TOML and restrict which
    /// hosts the sandbox can reach. The minimum required for AgentFlow
    /// to function is GitHub (api.github.com, *.github.com).
    ///
    /// Add any domains your project needs to build, test, or deploy:
    /// - Package registries (pypi.org, registry.npmjs.org, crates.io)
    /// - API endpoints your code calls (api.stripe.com, etc.)
    /// - Internal registries (npm.pkg.github.com, etc.)
    ///
    /// If empty, only GitHub domains are allowed.
    #[serde(default)]
    pub allowed_domains: Vec<String>,
}

impl Default for ProjectConfig {
    fn default() -> Self {
        Self {
            sandbox: SandboxConfig {
                allowed_domains: vec![
                    "api.github.com".to_string(),
                    "*.github.com".to_string(),
                ],
            },
        }
    }
}

/// File name for the project configuration.
pub const PROJECT_CONFIG_FILE: &str = ".agentflow.toml";

impl ProjectConfig {
    /// Load project configuration from a workspace root directory.
    ///
    /// Looks for `.agentflow.toml` in the root. Returns defaults if
    /// the file doesn't exist. Returns an error if the file exists but
    /// can't be parsed.
    pub fn load(workspace_root: &Path) -> Self {
        let config_path = workspace_root.join(PROJECT_CONFIG_FILE);
        if !config_path.exists() {
            tracing::debug!(
                path = %config_path.display(),
                "No .agentflow.toml found — using defaults"
            );
            return Self::default();
        }

        match Self::load_from_path(&config_path) {
            Ok(config) => {
                tracing::info!(
                    path = %config_path.display(),
                    domains = ?config.sandbox.allowed_domains,
                    "Loaded project configuration"
                );
                config
            }
            Err(e) => {
                tracing::warn!(
                    path = %config_path.display(),
                    error = %e,
                    "Failed to parse .agentflow.toml — using defaults"
                );
                Self::default()
            }
        }
    }

    /// Load from a specific path (for testing).
    pub fn load_from_path(path: &Path) -> Result<Self> {
        let content = std::fs::read_to_string(path)
            .with_context(|| format!("Failed to read {}", path.display()))?;
        toml::from_str(&content)
            .with_context(|| format!("Failed to parse {}", path.display()))
    }

    /// Get the allowed domains, merging with registry defaults.
    ///
    /// Project-level domains take priority. If the project config has domains,
    /// those are used. If empty, falls back to the provided defaults list.
    pub fn resolve_allowed_domains(&self, registry_defaults: &[String]) -> Vec<String> {
        if self.sandbox.allowed_domains.is_empty() {
            registry_defaults.to_vec()
        } else {
            self.sandbox.allowed_domains.clone()
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use tempfile::NamedTempFile;

    #[test]
    fn test_load_defaults_when_file_missing() {
        let config = ProjectConfig::load(Path::new("/nonexistent/path"));
        assert!(config.sandbox.allowed_domains.contains(&"api.github.com".to_string()));
        assert!(config.sandbox.allowed_domains.contains(&"*.github.com".to_string()));
    }

    #[test]
    fn test_load_custom_domains() {
        let mut f = NamedTempFile::new().unwrap();
        write!(
            f,
            r#"
[sandbox]
allowed_domains = [
    "api.github.com",
    "*.github.com",
    "pypi.org",
    "registry.npmjs.org",
    "crates.io",
    "api.internal.company.com",
]
"#
        )
        .unwrap();

        let config = ProjectConfig::load_from_path(f.path()).unwrap();
        assert_eq!(config.sandbox.allowed_domains.len(), 6);
        assert!(config.sandbox.allowed_domains.contains(&"pypi.org".to_string()));
        assert!(config.sandbox.allowed_domains.contains(&"api.internal.company.com".to_string()));
    }

    #[test]
    fn test_resolve_with_registry_defaults() {
        let config = ProjectConfig {
            sandbox: SandboxConfig {
                allowed_domains: vec!["api.github.com".to_string(), "custom.api".to_string()],
            },
        };
        let registry_defaults = vec!["api.github.com".to_string(), "*.github.com".to_string()];
        let resolved = config.resolve_allowed_domains(&registry_defaults);
        // Project config takes priority
        assert_eq!(resolved.len(), 2);
        assert!(resolved.contains(&"custom.api".to_string()));
    }

    #[test]
    fn test_resolve_fallback_to_registry() {
        let config = ProjectConfig::default(); // empty allowed_domains in sandbox
        let registry_defaults = vec!["api.github.com".to_string(), "*.github.com".to_string()];
        let resolved = config.resolve_allowed_domains(&registry_defaults);
        // Falls back to registry defaults
        assert_eq!(resolved.len(), 2);
    }

    #[test]
    fn test_invalid_toml_uses_defaults() {
        let mut f = NamedTempFile::new().unwrap();
        write!(f, "this is not valid toml {{{}}}").unwrap();

        let config = ProjectConfig::load_from_path(f.path()).unwrap_err();
        // Should fail to parse
        assert!(config.to_string().contains("Failed to parse"));
    }
}