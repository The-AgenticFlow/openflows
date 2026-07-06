// crates/pair-harness/src/mcp_config.rs
//! MCP configuration generation for pairs.
//!
//! Generates per-pair mcp.json files with environment-specific paths
//! and credentials.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::Path;
use tracing::{debug, info};

use crate::transport::WorkspaceTransport;

/// Generates MCP configuration files for pairs.
pub struct McpConfigGenerator {
    /// GitHub token for MCP tools
    github_token: String,
    /// Optional Redis URL for shared store (uses filesystem if None)
    redis_url: Option<String>,
}

impl McpConfigGenerator {
    /// Create a new MCP config generator without Redis (uses filesystem state).
    pub fn new(github_token: impl Into<String>, redis_url: Option<impl Into<String>>) -> Self {
        Self {
            github_token: github_token.into(),
            redis_url: redis_url.map(|s| s.into()),
        }
    }

    /// Build the FORGE mcp.json configuration value.
    fn build_forge_config(&self, worktree: &Path, shared: &Path) -> Value {
        json!({
            "mcpServers": {
                "github": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "env": {
                        "GITHUB_PERSONAL_ACCESS_TOKEN": self.github_token
                    }
                },
                "filesystem": {
                    "command": "npx",
                    "args": [
                        "-y",
                        "@modelcontextprotocol/server-filesystem",
                        worktree.to_string_lossy().to_string(),
                        shared.to_string_lossy().to_string()
                    ]
                },
                "shell": {
                    "command": "shell-mcp-server",
                    "args": [
                        "--allowlist",
                        "orchestration/agent/tooling/run-tests.sh,cargo clippy,cargo test,npx eslint,npx jest,ruff check"
                    ]
                }
            }
        })
    }

    /// Build the SENTINEL mcp.json configuration value (read-only access).
    fn build_sentinel_config(&self, worktree: &Path, shared: &Path) -> Value {
        json!({
            "mcpServers": {
                "github": {
                    "command": "npx",
                    "args": ["-y", "@modelcontextprotocol/server-github"],
                    "env": {
                        "GITHUB_PERSONAL_ACCESS_TOKEN": self.github_token
                    }
                },
                "filesystem": {
                    "command": "npx",
                    "args": [
                        "-y",
                        "@modelcontextprotocol/server-filesystem",
                        worktree.to_string_lossy().to_string(),
                        shared.to_string_lossy().to_string()
                    ]
                },
                "shell": {
                    "command": "shell-mcp-server",
                    "args": [
                        "--allowlist",
                        "orchestration/agent/tooling/run-tests.sh,npx eslint,ruff check,cargo clippy"
                    ]
                }
            }
        })
    }

    /// Generate FORGE's mcp.json (local filesystem write).
    ///
    /// Kept for local-mode convenience and unit tests. Production code in
    /// Coder mode must use [`Self::generate_forge_config_via_transport`],
    /// because the `output_path` is a path inside the remote Coder workspace
    /// and cannot be written to via the local `std::fs`.
    pub fn generate_forge_config(
        &self,
        worktree: &Path,
        shared: &Path,
        output_path: &Path,
    ) -> Result<()> {
        info!(path = %output_path.display(), "Generating FORGE mcp.json");
        let config = self.build_forge_config(worktree, shared);
        self.write_config(output_path, &config)
    }

    /// Generate SENTINEL's mcp.json (local filesystem write; read-only access).
    ///
    /// See [`Self::generate_forge_config`] for why the transport variant
    /// should be used in Coder mode.
    pub fn generate_sentinel_config(
        &self,
        worktree: &Path,
        shared: &Path,
        output_path: &Path,
    ) -> Result<()> {
        info!(path = %output_path.display(), "Generating SENTINEL mcp.json");
        let config = self.build_sentinel_config(worktree, shared);
        self.write_config(output_path, &config)
    }

    /// Generate FORGE's mcp.json, writing through a [`WorkspaceTransport`].
    ///
    /// Use this in Coder mode (and it is equally valid in local mode via
    /// `LocalTransport`): the `output_path` resolves to a path inside the
    /// workspace (e.g. `/home/coder/workspace/.claude/mcp.json`), which only
    /// exists remotely. The transport takes care of creating parent
    /// directories and writing the file in the correct location.
    pub async fn generate_forge_config_via_transport(
        &self,
        worktree: &Path,
        shared: &Path,
        output_path: &Path,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        info!(path = %output_path.display(), "Generating FORGE mcp.json");
        let config = self.build_forge_config(worktree, shared);
        self.write_config_via_transport(output_path, &config, transport)
            .await
    }

    /// Generate SENTINEL's mcp.json (read-only access), writing through a
    /// [`WorkspaceTransport`]. See [`Self::generate_forge_config_via_transport`].
    pub async fn generate_sentinel_config_via_transport(
        &self,
        worktree: &Path,
        shared: &Path,
        output_path: &Path,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        info!(path = %output_path.display(), "Generating SENTINEL mcp.json");
        let config = self.build_sentinel_config(worktree, shared);
        self.write_config_via_transport(output_path, &config, transport)
            .await
    }

    /// Write config to file atomically.
    fn write_config(&self, path: &Path, config: &Value) -> Result<()> {
        // Ensure parent directory exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).context("Failed to create mcp.json parent directory")?;
        }

        // Write to temp file first
        let temp_path = path.with_extension("json.tmp");
        let content =
            serde_json::to_string_pretty(config).context("Failed to serialize mcp.json")?;

        fs::write(&temp_path, content).context("Failed to write mcp.json")?;

        // Atomic rename
        fs::rename(&temp_path, path).context("Failed to rename mcp.json")?;

        debug!(path = %path.display(), "MCP config written");
        Ok(())
    }

    /// Write config to a workspace through a [`WorkspaceTransport`].
    ///
    /// The transport is responsible for creating parent directories (both
    /// `LocalTransport` and `CoderTransport` do so internally), so this does
    /// not need a separate `create_dir_all` call. Using the transport keeps
    /// the write inside the correct workspace when running in Coder mode,
    /// where `path` resolves to a remote location (e.g.
    /// `/home/coder/workspace/.claude/mcp.json`) that does not exist on the
    /// local host.
    async fn write_config_via_transport(
        &self,
        path: &Path,
        config: &Value,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let content =
            serde_json::to_string_pretty(config).context("Failed to serialize mcp.json")?;

        transport
            .write_file(&path.to_string_lossy(), &content)
            .await
            .context("Failed to write mcp.json via transport")?;

        debug!(path = %path.display(), "MCP config written via transport");
        Ok(())
    }

    /// Generate mcp.json from a template with variable substitution.
    pub fn generate_from_template(
        &self,
        template_path: &Path,
        worktree: &Path,
        shared: &Path,
        output_path: &Path,
    ) -> Result<()> {
        info!(
            template = %template_path.display(),
            output = %output_path.display(),
            "Generating mcp.json from template"
        );

        let template =
            fs::read_to_string(template_path).context("Failed to read mcp.json template")?;

        // Substitute variables
        let redis_url_str = self.redis_url.as_deref().unwrap_or("");
        let config = template
            .replace("${SPRINTLESS_GITHUB_TOKEN}", &self.github_token)
            .replace("${SPRINTLESS_REDIS_URL}", redis_url_str)
            .replace("${SPRINTLESS_WORKTREE}", &worktree.to_string_lossy())
            .replace("${SPRINTLESS_SHARED}", &shared.to_string_lossy());

        // Parse to validate JSON
        let _: Value =
            serde_json::from_str(&config).context("Generated mcp.json is not valid JSON")?;

        // Write atomically
        let temp_path = output_path.with_extension("json.tmp");
        fs::write(&temp_path, config).context("Failed to write mcp.json")?;
        fs::rename(&temp_path, output_path).context("Failed to rename mcp.json")?;

        Ok(())
    }
}

/// Default MCP configuration template.
pub const DEFAULT_MCP_TEMPLATE: &str = r#"{
  "mcpServers": {
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "${SPRINTLESS_GITHUB_TOKEN}"
      }
    },
    "filesystem": {
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-filesystem",
        "${SPRINTLESS_WORKTREE}",
        "${SPRINTLESS_SHARED}"
      ]
    },
    "shell": {
      "command": "shell-mcp-server",
      "args": [
        "--allowlist",
        "orchestration/agent/tooling/run-tests.sh,cargo clippy,cargo test,npx eslint,npx jest,ruff check"
      ]
    }
  }
}"#;

/// Default MCP configuration template for SENTINEL (read-only).
pub const DEFAULT_SENTINEL_MCP_TEMPLATE: &str = r#"{
  "mcpServers": {
    "github": {
      "command": "npx",
      "args": ["-y", "@modelcontextprotocol/server-github"],
      "env": {
        "GITHUB_PERSONAL_ACCESS_TOKEN": "${SPRINTLESS_GITHUB_TOKEN}"
      }
    },
    "filesystem": {
      "command": "npx",
      "args": [
        "-y",
        "@modelcontextprotocol/server-filesystem",
        "${SPRINTLESS_WORKTREE}",
        "${SPRINTLESS_SHARED}"
      ]
    },
    "shell": {
      "command": "shell-mcp-server",
      "args": [
        "--allowlist",
        "orchestration/agent/tooling/run-tests.sh,npx eslint,ruff check,cargo clippy"
      ]
    }
  }
}"#;

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_generate_forge_config() {
        let dir = tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        let shared = dir.path().join("shared");
        let output = dir.path().join("mcp.json");

        let generator = McpConfigGenerator::new("test-token", Some("redis://localhost"));
        generator
            .generate_forge_config(&worktree, &shared, &output)
            .unwrap();

        let content = fs::read_to_string(&output).unwrap();
        let config: Value = serde_json::from_str(&content).unwrap();

        assert!(config["mcpServers"]["github"].is_object());
        assert!(config["mcpServers"]["filesystem"].is_object());
        assert!(config["mcpServers"]["shell"].is_object());

        // FORGE should have filesystem access to both worktree and shared
        let fs_args = config["mcpServers"]["filesystem"]["args"]
            .as_array()
            .unwrap();
        // Should have both worktree and shared paths
        assert_eq!(fs_args.len(), 4); // -y, server-filesystem, worktree, shared
    }

    #[test]
    fn test_generate_sentinel_config() {
        let dir = tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        let shared = dir.path().join("shared");
        let output = dir.path().join("mcp.json");

        let generator = McpConfigGenerator::new("test-token", Some("redis://localhost"));
        generator
            .generate_sentinel_config(&worktree, &shared, &output)
            .unwrap();

        let content = fs::read_to_string(&output).unwrap();
        let config: Value = serde_json::from_str(&content).unwrap();

        // SENTINEL has filesystem access to both worktree and shared
        let fs_args = config["mcpServers"]["filesystem"]["args"]
            .as_array()
            .unwrap();
        // Should NOT have --read-only (SENTINEL needs to write reviews)
        assert!(!fs_args.iter().any(|a| a == "--read-only"));
        // Should have both worktree and shared paths
        assert_eq!(fs_args.len(), 4); // -y, server-filesystem, worktree, shared
    }
}
