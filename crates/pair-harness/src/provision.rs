// crates/pair-harness/src/provision.rs
//! Provisioning for pair configuration files.
//!
//! Generates settings.json for FORGE and SENTINEL with auto-mode
//! permissions and explicit allow/deny lists.
//! Also generates Codex-native config (.codex/, .agents/, AGENTS.md).

use anyhow::{Context, Result};
use serde_json::{json, Value};
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{debug, info};

use crate::pair_state::{PairArtifact, PairStateStore};
use crate::process::{get_backend_config, BackendConfig};
use crate::transport::WorkspaceTransport;
use crate::types::{CliBackend, WorkspaceProvider};

/// Resolve the `coder_module` for a given agent role.
/// Reads from `registry.json` if present, falls back to `DEFAULT_AGENT_MODULES`.
/// Returns `None` in Local mode or if no Coder module is applicable.
fn resolve_coder_module_for_role(
    orchestrator_dir: &Path,
    cli_backend: CliBackend,
    role: &str,
    workspace_provider: WorkspaceProvider,
) -> Option<config::registry::CoderModule> {
    if workspace_provider != WorkspaceProvider::Coder {
        return None;
    }

    let registry_path = orchestrator_dir
        .join("orchestration")
        .join("agent")
        .join("registry.json");

    if let Ok(registry) = config::Registry::load(&registry_path) {
        let base_id = registry.normalize_agent_id(role);
        if let Some(entry) = registry.get(base_id) {
            return Some(entry.resolve_coder_module());
        }
    }

    // Fallback to default mapping based on CLI
    let cli_str = cli_backend.as_str();
    let (source, version) = config::registry::DEFAULT_AGENT_MODULES
        .iter()
        .find(|(key, _, _)| *key == cli_str)
        .map(|(_, source, version)| (source.to_string(), version.to_string()))
        .unwrap_or_else(|| (
            "registry.coder.com/coder/claude-code/coder".to_string(),
            "5.2.0".to_string(),
        ));

    let permission_mode = config::registry::default_permission_mode_for_role(role);
    let mut params = serde_json::Map::new();
    params.insert("workdir".to_string(), serde_json::Value::String("/home/coder/workspace".to_string()));
    params.insert("permission_mode".to_string(), serde_json::Value::String(permission_mode.to_string()));
    Some(config::registry::CoderModule::with_params(
        source,
        version,
        serde_json::Value::Object(params),
    ))
}

struct ExtrasContext<'a> {
    redis_url: Option<&'a str>,
    model_backend: Option<&'a str>,
}

/// Provisions configuration files for pairs.
pub struct Provisioner {
    /// Project root directory
    project_root: PathBuf,
    /// Workspace provider mode (Local or Coder)
    workspace_provider: WorkspaceProvider,
}

impl Provisioner {
    /// Create a new provisioner.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
            workspace_provider: WorkspaceProvider::Local,
        }
    }

    /// Create a provisioner with explicit workspace provider mode.
    pub fn with_provider(project_root: impl Into<PathBuf>, provider: WorkspaceProvider) -> Self {
        Self {
            project_root: project_root.into(),
            workspace_provider: provider,
        }
    }

    /// Resolve the orchestrator source directory.
    ///
    /// The AgentFlow source repo (containing `orchestration/`) is NOT always
    /// at `project_root` — when running against a target workspace, project_root
    /// points to the cloned target repo. The `ORCHESTRATOR_DIR` env var (set
    /// by the agentflow binary at startup) points to the AgentFlow source root.
    fn orchestrator_dir(&self) -> PathBuf {
        if let Ok(orch_dir) = std::env::var("ORCHESTRATOR_DIR") {
            PathBuf::from(orch_dir)
        } else {
            self.project_root.clone()
        }
    }

    /// Load allowed domains for an agent from the registry.
    /// Falls back to registry-level defaults, then to the minimum (GitHub only).
    fn resolve_allowed_domains(&self, pair_id: &str) -> Vec<String> {
        let registry_path = self
            .orchestrator_dir()
            .join("orchestration")
            .join("agent")
            .join("registry.json");
        match config::Registry::load(&registry_path) {
            Ok(registry) => {
                let base_id = registry.normalize_agent_id(pair_id);
                match registry.get(base_id) {
                    Some(entry) => entry
                        .resolve_allowed_domains(&registry.allowed_domains)
                        .to_vec(),
                    None => registry.allowed_domains.clone(),
                }
            }
            Err(_) => {
                // Fallback: GitHub only
                vec!["api.github.com".to_string(), "*.github.com".to_string()]
            }
        }
    }

    /// Provision all configuration for a pair using BackendConfig.
    #[allow(clippy::too_many_arguments)]
    pub async fn provision_pair(
        &self,
        pair_id: &str,
        worktree: &Path,
        shared: &Path,
        transport: &dyn WorkspaceTransport,
        github_token: &str,
        redis_url: Option<&str>,
        cli_backend: CliBackend,
        model_backend: Option<&str>,
    ) -> Result<()> {
        info!(pair = pair_id, backend = ?cli_backend, "Provisioning pair configuration");

        let backend_config = get_backend_config(cli_backend, worktree, shared);

        // 1. Create FORGE settings/config
        self.create_forge_settings(worktree, &backend_config, transport).await?;

        // 2. Remove legacy sentinel dir if present (always local path in orchestrator)
        let legacy_dir = shared.join("sentinel");
        if legacy_dir.exists() {
            transport
                .remove_dir_all(&legacy_dir.to_string_lossy())
                .await
                .ok();
        }

        // 3. Create SENTINEL settings/config
        self.create_sentinel_settings(shared, &backend_config, transport).await?;

        // 4. Create FORGE mcp.json (if backend uses MCP config)
        let is_codex_non_responses =
            cli_backend == CliBackend::Codex && crate::process::codex_use_sse();
        if !backend_config.mcp_config_rel.as_os_str().is_empty() && !is_codex_non_responses {
            let mcp_gen = crate::mcp_config::McpConfigGenerator::new(github_token, redis_url);
            let mcp_path = backend_config.mcp_config_path(worktree);
            mcp_gen.generate_forge_config(worktree, shared, &mcp_path)?;
        }

        // 5. Create SENTINEL mcp.json
        if !backend_config.mcp_config_rel.as_os_str().is_empty() && !is_codex_non_responses {
            let mcp_gen = crate::mcp_config::McpConfigGenerator::new(github_token, redis_url);
            let mcp_path = backend_config.mcp_config_path(shared);
            mcp_gen.generate_sentinel_config(worktree, shared, &mcp_path)?;
        }

        // 6. Symlink/copy plugin to FORGE
        self.symlink_plugin(worktree, "forge", &backend_config, transport).await?;

        // 7. Symlink/copy plugin to SENTINEL
        self.symlink_plugin(shared, "sentinel", &backend_config, transport).await?;

        // 8. Create shared directory structure
        self.create_shared_structure(shared, transport).await?;

        // 9. Backend-specific extras (hooks, permissions, AGENTS.md, skills)
        if backend_config.needs_extras_provisioning {
            self.provision_backend_extras(
                pair_id,
                &backend_config,
                worktree,
                shared,
                github_token,
                &ExtrasContext {
                    redis_url,
                    model_backend,
                },
                transport,
            ).await?;
        }

        // 10. In Coder mode, resolve coder_module and generate Terraform variables
        if self.workspace_provider == WorkspaceProvider::Coder {
            self.provision_coder_terraform_vars(pair_id, worktree, shared, cli_backend, model_backend, transport).await?;
            self.write_standards_files(worktree, transport).await?;
        }

        info!(pair = pair_id, backend = ?cli_backend, "Pair provisioning complete");
        Ok(())
    }

    /// Generate Terraform variable files from the resolved coder_module configuration.
    /// Creates `coder.auto.tfvars` in the worktree with module source, version, and params.
    async fn provision_coder_terraform_vars(
        &self,
        pair_id: &str,
        worktree: &Path,
        _shared: &Path,
        cli_backend: CliBackend,
        model_backend: Option<&str>,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let forge_module = resolve_coder_module_for_role(
            &self.orchestrator_dir(),
            cli_backend,
            "forge",
            WorkspaceProvider::Coder, // We're in this method because workspace_provider == Coder
        );

        if let Some(ref module) = forge_module {
            let tfvars = format!(
                r#"# Auto-generated by AgentFlow Provisioner
# DO NOT EDIT — changes will be overwritten on next provision

coder_module_source   = "{}"
coder_module_version  = "{}"
coder_workdir         = "{}"
coder_permission_mode = "{}"
coder_ai_gateway      = {}
{}
"#,
                module.source,
                module.version,
                module.workdir(),
                module.permission_mode(),
                module.ai_gateway_enabled(),
                if let Some(m) = model_backend {
                    format!("model_backend         = \"{}\"", m)
                } else {
                    String::new()
                }
            );

            let tfvars_path = worktree.join("coder.auto.tfvars");
            transport
                .write_file(&tfvars_path.to_string_lossy(), &tfvars)
                .await
                .context("Failed to write coder.auto.tfvars")?;

            info!(
                pair = pair_id,
                module_source = %module.source,
                module_version = %module.version,
                "Generated Coder Terraform variables"
            );
        }

        Ok(())
    }

    /// Provision backend-specific extras (hooks, permissions, AGENTS.md, skills).
    async fn provision_backend_extras(
        &self,
        pair_id: &str,
        backend_config: &BackendConfig,
        worktree: &Path,
        shared: &Path,
        github_token: &str,
        ctx: &ExtrasContext<'_>,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let is_codex = backend_config.mcp_config_rel.starts_with(".codex");
        let redis_url = ctx.redis_url;
        let model_backend = ctx.model_backend;

        if is_codex {
            self.generate_codex_config_toml(
                worktree, worktree, shared,
                github_token, redis_url, "danger-full-access",
                transport,
            ).await?;
            self.generate_codex_config_toml(
                shared, worktree, shared,
                github_token, redis_url, "read-only",
                transport,
            ).await?;
            self.generate_codex_agent_tomls(worktree, model_backend, transport).await?;
            self.generate_codex_agent_toml_for_role(shared, "sentinel", model_backend, transport).await?;
            self.generate_codex_hooks_json(worktree, shared, transport).await?;
            self.deploy_codex_plugin(worktree, transport).await?;
            self.deploy_codex_plugin(shared, transport).await?;
            let forge_domains = self.resolve_allowed_domains(pair_id);
            self.generate_codex_permissions(
                worktree, shared, "danger-full-access", &forge_domains, transport,
            ).await?;
            self.generate_codex_permissions(
                shared, shared, "read-only", &[], transport,
            ).await?;
            self.symlink_skills_to_agents(worktree, transport).await?;
            self.symlink_skills_to_agents_for_role(shared, "sentinel", transport).await?;
        } else {
            self.generate_claude_hooks_json(worktree, shared, transport).await?;
            self.symlink_skills_to_claude(worktree, transport).await?;
            self.symlink_skills_to_claude_for_role(shared, "sentinel", transport).await?;
            let _ = self.enhance_claude_permissions(worktree, shared);
        }

        self.write_agents_md(worktree, "forge")?;
        self.write_agents_md(shared, "sentinel")?;

        Ok(())
    }

    /// Create FORGE's settings.json with auto-mode permissions.
    pub async fn create_forge_settings(
        &self,
        worktree: &Path,
        config: &BackendConfig,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let settings_dir =
            worktree.join(config.settings_rel.parent().unwrap_or(&config.settings_rel));
        transport.create_dir_all(&settings_dir.to_string_lossy()).await.context("Failed to create settings directory")?;

        let settings_path = config.settings_path(worktree);

        info!(path = %settings_path.display(), "Creating FORGE settings");

        // Minimal settings - permissions are handled by --dangerously-skip-permissions flag
        let settings = json!({
            "permissions": {
                "defaultMode": "auto"
            }
        });

        self.write_json_via_transport(settings_path.clone(), &settings, transport).await?;

        self.ensure_worktree_gitignore_via_transport(worktree, config, transport).await
    }

    async fn ensure_worktree_gitignore_via_transport(
        &self,
        worktree: &Path,
        config: &BackendConfig,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let gitignore_path = worktree.join(".gitignore");
        let settings_dir_name = config
            .settings_rel
            .parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| ".claude/".to_string());
        let settings_entry = format!("{}/", settings_dir_name);
        // The shared orchestration directory (.pair-shared/) lives inside
        // the worktree so it is writable under the Codex sandbox.  It must
        // be gitignored so coordination files (PLAN.md, WORKLOG.md, etc.)
        // never end up in commits.
        let shared_entry = format!("{}/", crate::types::PairConfig::SHARED_DIR_NAME);
        let home_entry = if config.home_dir_suffix.is_empty() {
            None
        } else {
            Some(format!("{}/", config.home_dir_suffix))
        };

        let existing = transport
            .read_file(&gitignore_path.to_string_lossy())
            .await
            .unwrap_or_default();

        let mut updated = existing.clone();
        let mut entries: Vec<&str> = vec![&settings_entry, &shared_entry];
        if let Some(ref home) = home_entry {
            entries.push(home);
        }
        for entry in &entries {
            if !updated.lines().any(|l| l.trim() == *entry) {
                if updated.is_empty() {
                    updated = format!("{}\n", entry);
                } else if updated.ends_with('\n') {
                    updated = format!("{}{}\n", updated, entry);
                } else {
                    updated = format!("{}\n{}\n", updated, entry);
                }
            }
        }

        if updated != existing {
            transport
                .write_file(&gitignore_path.to_string_lossy(), &updated)
                .await
                .context("Failed to update .gitignore with settings directory exclusion")?;
            info!(
                path = %gitignore_path.display(),
                "Updated worktree .gitignore with runtime directories"
            );
        }

        Ok(())
    }

    /// Create SENTINEL's settings.json with read-only permissions.
    pub async fn create_sentinel_settings(
        &self,
        shared: &Path,
        config: &BackendConfig,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let legacy_dir = shared.join("sentinel");
        if legacy_dir.exists() {
            transport
                .remove_dir_all(&legacy_dir.to_string_lossy())
                .await
                .context("Failed to remove legacy sentinel directory")?;
        }

        let settings_dir =
            shared.join(config.settings_rel.parent().unwrap_or(&config.settings_rel));
        transport
            .create_dir_all(&settings_dir.to_string_lossy())
            .await
            .context("Failed to create sentinel settings directory")?;

        let settings_path = config.settings_path(shared);

        info!(path = %settings_path.display(), "Creating SENTINEL settings");

        // Minimal settings - permissions are handled by --dangerously-skip-permissions flag
        let settings = json!({
            "permissions": {
                "defaultMode": "auto"
            }
        });

        self.write_json_via_transport(settings_path.clone(), &settings, transport).await?;

        Ok(())
    }

    /// Symlink (or copy in Coder mode) the orchestration plugin to the backend-specific plugin directory.
    /// Uses the transport for the target operation so it works for both local and Coder workspaces.
    pub async fn symlink_plugin(
        &self,
        target_dir: &Path,
        role: &str,
        backend_config: &BackendConfig,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let plugin_source = self.orchestrator_dir().join("orchestration").join("plugin");

        // Check if plugin source exists (local filesystem — always orchestrator-local)
        if !plugin_source.exists() {
            debug!(
                role = role,
                path = %plugin_source.display(),
                "Plugin directory not found, skipping"
            );
            return Ok(());
        }

        // Compute intermediate plugins directory (without the final "orchestration" segment)
        let plugins_intermediate =
            target_dir.join(backend_config.plugin_dir_rel.parent().unwrap_or_else(|| {
                std::path::Path::new(".claude/plugins")
            }));

        // Compute the full absolute target path including the "orchestration" segment
        let plugins_target = plugins_intermediate.join("orchestration");

        // Ensure parent directories exist at the absolute target location
        transport
            .create_dir_all(&plugins_target.parent().unwrap_or(&plugins_target).to_string_lossy())
            .await
            .context("Failed to create plugins directory")?;

        transport
            .symlink_or_copy(&plugin_source, &plugins_target.to_string_lossy())
            .await
            .context("Failed to symlink_or_copy plugin")?;

        debug!(
            role = role,
            source = %plugin_source.display(),
            target = %plugins_target.display(),
            "Plugin symlinked/copied"
        );

        Ok(())
    }

    /// Create the shared directory structure.
    pub async fn create_shared_structure(
        &self,
        shared: &Path,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let already_exists = shared.exists();

        transport
            .execute(&format!("mkdir -p {}", shell_quote(shared)))
            .await
            .context("Failed to create shared directory")?;

        // Clean up the legacy sentinel subdirectory from older runs.
        let legacy_dir = shared.join("sentinel");
        if legacy_dir.exists() {
            transport
                .execute(&format!("rm -rf {}", shell_quote(&legacy_dir)))
                .await
                .context("Failed to remove legacy sentinel directory")?;
        }

        // The shared directory is now inside the worktree and covered by the
        // worktree's .gitignore, so we no longer need a per-directory
        // .gitignore here.  However, keep one for backward compatibility
        // with existing checkouts that still reference the old
        // `orchestration/pairs/` path.
        let gitignore = shared.join(".gitignore");
        if !gitignore.exists() {
            transport
                .write_file(
                    &gitignore.to_string_lossy(),
                    "# Shared artifacts are runtime state, not committed\n*\n!.gitignore\n",
                )
                .await
                .context("Failed to write .gitignore")?;
        }

        // On re-provision (e.g. CI fix, conflict rework), append a session
        // marker to WORKLOG.md rather than wiping it.  This preserves the
        // FORGE agent's progress notes from previous sessions, which are
        // valuable for debugging and for the resume prompt.  The watchdog
        // will see the updated mtime and not declare the pair stalled.
        if already_exists {
            let worklog_path = shared.join("WORKLOG.md");
            let existing = transport
                .read_file(&worklog_path.to_string_lossy())
                .await
                .unwrap_or_default();
            let marker = format!(
                "\n---\n\n## Session Restart ({})\n\n",
                chrono::Local::now().format("%Y-%m-%d %H:%M")
            );
            transport
                .write_file(
                    &worklog_path.to_string_lossy(),
                    &format!("{}{}", existing, marker),
                )
                .await
                .context("Failed to append session marker to WORKLOG.md")?;
            debug!(path = %worklog_path.display(), "Appended session restart marker to WORKLOG.md");
        }

        debug!(path = %shared.display(), "Shared directory structure created");
        Ok(())
    }

    /// Write JSON to file atomically.
    fn write_json(&self, path: &Path, value: &Value) -> Result<()> {
        let temp_path = path.with_extension("json.tmp");
        let content = serde_json::to_string_pretty(value).context("Failed to serialize JSON")?;

        fs::write(&temp_path, content).context("Failed to write JSON")?;

        fs::rename(&temp_path, path).context("Failed to rename JSON file")?;

        Ok(())
    }

    /// Write JSON to file via transport (supports Coder workspaces).
    async fn write_json_via_transport(
        &self,
        path: PathBuf,
        value: &Value,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let content = serde_json::to_string_pretty(value).context("Failed to serialize JSON")?;
        transport
            .write_file(&path.to_string_lossy(), &content)
            .await
            .context("Failed to write JSON via transport")?;
        Ok(())
    }

    /// Generate .codex/config.toml for a given directory.
    async fn generate_codex_config_toml(
        &self,
        target: &Path,
        worktree: &Path,
        shared: &Path,
        github_token: &str,
        redis_url: Option<&str>,
        sandbox_mode: &str,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let codex_dir = target.join(".codex");
        transport.create_dir_all(&codex_dir.to_string_lossy()).await.context("Failed to create .codex directory")?;

        let config_path = codex_dir.join("config.toml");

        let network_access =
            sandbox_mode == "workspace-write" || sandbox_mode == "danger-full-access";
        let approval_policy = if sandbox_mode == "read-only" {
            // SENTINEL runs in --ephemeral mode with no interactive terminal,
            // so it must run autonomously. "never" allows the agent to proceed
            // without approval prompts. Valid Codex values are:
            // untrusted, on-failure, on-request, granular, never
            "never"
        } else {
            "on-request"
        };

        let _redis_url_val = redis_url.unwrap_or("");
        let mcp_shell_args = if sandbox_mode == "workspace-write"
            || sandbox_mode == "danger-full-access"
        {
            r#""orchestration/agent/tooling/run-tests.sh,cargo clippy,cargo test,npx eslint,npx jest,ruff check""#
        } else {
            r#""orchestration/agent/tooling/run-tests.sh,npx eslint,ruff check,cargo clippy""#
        };

        // When the endpoint doesn't support the Responses API (probed at startup),
        // MCP servers must be excluded from config.toml because:
        // 1. MCP tools are registered with type="mcp" in the Responses API
        // 2. Non-OpenAI providers reject non-function tool types with 400 errors
        // 3. The filesystem MCP server also has an npm dependency issue (missing zod)
        // MCP servers are only compatible with providers that support the full
        // Responses API tool type schema (i.e., OpenAI's own endpoints).
        let include_mcp_servers = !crate::process::codex_use_sse();

        let config = if include_mcp_servers {
            format!(
                r#"# Auto-generated by AgentFlow Provisioner
# DO NOT EDIT — changes will be overwritten on next provision

approval_policy = "{approval_policy}"
sandbox_mode = "{sandbox_mode}"

[sandbox_{sandbox_mode}]
network_access = {network_access}

[mcp_servers.github]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-github"]

[mcp_servers.github.env]
GITHUB_PERSONAL_ACCESS_TOKEN = "{github_token}"

[mcp_servers.filesystem]
command = "npx"
args = ["-y", "@modelcontextprotocol/server-filesystem", "{worktree}", "{shared}"]

[mcp_servers.shell]
command = "shell-mcp-server"
args = ["--allowlist", {mcp_shell_args}]

[agents]
max_threads = 6
max_depth = 1
"#,
                worktree = worktree.display(),
                shared = shared.display(),
            )
        } else {
            format!(
                r#"# Auto-generated by AgentFlow Provisioner
# DO NOT EDIT — changes will be overwritten on next provision
#
# MCP servers are DISABLED because the endpoint does not natively support
# the Responses API (/v1/responses returned 404). A local proxy translates
# between Responses API and Chat Completions format. Non-OpenAI providers
# reject MCP tool types (type="mcp") in the Responses API, causing
# "unknown tool type" 400 errors. GitHub operations are available via
# the FORGE/SENTINEL agent's built-in tools and gh CLI instead.

approval_policy = "{approval_policy}"
sandbox_mode = "{sandbox_mode}"

[sandbox_{sandbox_mode}]
network_access = {network_access}

[agents]
max_threads = 6
max_depth = 1
"#,
            )
        };

        transport.write_file(&config_path.to_string_lossy(), &config).await.context("Failed to write .codex/config.toml")?;
        info!(path = %config_path.display(), "Codex config.toml generated");
        Ok(())
    }

    /// Generate .codex/agents/*.toml from existing agent.md files.
    async fn generate_codex_agent_tomls(
        &self,
        worktree: &Path,
        model_backend: Option<&str>,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let agent_ids = ["forge", "sentinel"];

        for agent_id in &agent_ids {
            self.generate_codex_agent_toml_for_role(worktree, agent_id, model_backend, transport).await?;
        }

        Ok(())
    }

    /// Generate a single .codex/agents/{role}.toml in the target directory.
    async fn generate_codex_agent_toml_for_role(
        &self,
        target: &Path,
        agent_id: &str,
        model_backend: Option<&str>,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let agents_dir = target.join(".codex").join("agents");
        transport.create_dir_all(&agents_dir.to_string_lossy()).await.context("Failed to create .codex/agents directory")?;

        let agent_md_path = self
            .orchestrator_dir()
            .join("orchestration")
            .join("agent")
            .join("agents")
            .join(format!("{}.agent.md", agent_id));

        if !agent_md_path.exists() {
            debug!(
                path = %agent_md_path.display(),
                "Agent persona file not found, skipping TOML generation"
            );
            return Ok(());
        }

        let persona = fs::read_to_string(&agent_md_path)
            .context(format!("Failed to read {}", agent_md_path.display()))?;

        let (role, sandbox_mode) = match agent_id {
            "forge" => ("builder", "danger-full-access"),
            "sentinel" => ("reviewer", "read-only"),
            _ => ("unknown", "danger-full-access"),
        };

        // Resolve model: registry model_backend (highest priority) > FIREWORKS_MODEL > OPENAI_MODEL > default
        // Provider prefixes (e.g. "anthropic/", "openai/") must be stripped
        // because Codex CLI expects bare model names.
        let model = model_backend
            .map(|m| crate::process::strip_provider_prefix(m).to_string())
            .or_else(|| {
                std::env::var("FIREWORKS_MODEL")
                    .ok()
                    .map(|m| crate::process::strip_provider_prefix(&m).to_string())
            })
            .or_else(|| {
                std::env::var("OPENAI_MODEL")
                    .ok()
                    .map(|m| crate::process::strip_provider_prefix(&m).to_string())
            })
            .unwrap_or_else(|| "gpt-4o-mini".to_string());

        let toml_content = format!(
            r#"# Auto-generated by AgentFlow Provisioner
# Source: {source}
# DO NOT EDIT — changes will be overwritten on next provision

name = "{id}"
description = "{role} agent for AgentFlow orchestration"
model = "{model}"
sandbox_mode = "{sandbox}"

developer_instructions = """
{persona}
"""
"#,
            id = agent_id,
            role = role,
            model = model,
            sandbox = sandbox_mode,
            persona = persona,
            source = agent_md_path.display(),
        );

        let toml_path = agents_dir.join(format!("{}.toml", agent_id));
        transport
            .write_file(&toml_path.to_string_lossy(), &toml_content)
            .await
            .context(format!("Failed to write {}", toml_path.display()))?;
        info!(path = %toml_path.display(), "Codex agent TOML generated for {} in {}", agent_id, target.display());
        Ok(())
    }

    /// Generate .codex/hooks.json with relative paths to locally-installed hook scripts.
    async fn generate_codex_hooks_json(
        &self,
        worktree: &Path,
        shared: &Path,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let hooks_source = self
            .orchestrator_dir()
            .join("orchestration")
            .join("plugin")
            .join("hooks");

        if !hooks_source.exists() {
            debug!("Hooks source directory not found, skipping hooks.json generation");
            return Ok(());
        }

        // Install hook scripts into FORGE worktree
        self.install_hook_scripts(worktree, "forge", &hooks_source, transport).await?;

        // Generate FORGE hooks.json (referencing local copies)
        let forge_hooks = self.build_codex_hooks_json("forge", &hooks_source)?;
        let forge_hooks_path = worktree.join(".codex").join("hooks.json");
        transport
            .create_dir_all(&forge_hooks_path.parent().unwrap().to_string_lossy())
            .await?;
        self.write_json_via_transport(forge_hooks_path, &forge_hooks, transport).await?;
        info!("Codex hooks.json generated for FORGE");

        // Install hook scripts into SENTINEL shared dir
        self.install_hook_scripts(shared, "sentinel", &hooks_source, transport).await?;

        // Generate SENTINEL hooks.json (referencing local copies)
        let sentinel_hooks = self.build_codex_hooks_json("sentinel", &hooks_source)?;
        let sentinel_hooks_path = shared.join(".codex").join("hooks.json");
        transport
            .create_dir_all(&sentinel_hooks_path.parent().unwrap().to_string_lossy())
            .await?;
        self.write_json_via_transport(sentinel_hooks_path, &sentinel_hooks, transport).await?;
        info!("Codex hooks.json generated for SENTINEL");

        Ok(())
    }

    /// Copy hook scripts from the source repo into .codex/hooks/{role}/ in the target directory.
    ///
    /// This makes the harness self-contained so it doesn't depend on the source
    /// repo remaining at the same absolute path at runtime.
    async fn install_hook_scripts(
        &self,
        target: &Path,
        role: &str,
        hooks_source: &Path,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let hook_names: Vec<&str> = match role {
            "forge" => vec![
                "session_start",
                "pre_bash_guard",
                "pre_write_check",
                "post_write_lint",
                "pre_compact_handoff",
                "stop_require_artifact",
                "subagent_start",
                "subagent_stop",
            ],
            "sentinel" => vec![
                "session_start",
                "pre_bash_readonly_guard",
                "post_write_validate",
                "stop_require_eval",
                "subagent_start",
                "subagent_stop",
            ],
            _ => vec![],
        };

        let hooks_dest = target.join(".codex").join("hooks").join(role);
        transport
            .create_dir_all(&hooks_dest.to_string_lossy())
            .await
            .context("Failed to create hooks directory")?;

        for hook_name in &hook_names {
            let src = hooks_source.join(role).join(format!("{}.sh", hook_name));
            if !src.exists() {
                debug!(
                    path = %src.display(),
                    "Hook script not found in source, skipping copy"
                );
                continue;
            }

            // Read the source file from local filesystem and write via transport
            let content = fs::read_to_string(&src)
                .context(format!("Failed to read hook source {}", src.display()))?;

            let dst_path = hooks_dest.join(format!("{}.sh", hook_name));
            transport
                .write_file(&dst_path.to_string_lossy(), &content)
                .await
                .context(format!("Failed to write hook script {}", hook_name))?;

            // Ensure executable bit via chmod in the workspace
            let _ = transport
                .execute(&format!("chmod +x {}", shell_quote(&dst_path)))
                .await;

            debug!(
                src = %src.display(),
                dst = %dst_path.display(),
                "Hook script copied"
            );
        }

        info!(
            path = %hooks_dest.display(),
            role = role,
            "Hook scripts installed"
        );
        Ok(())
    }

    /// Build the Codex hooks.json structure for a given agent role.
    fn build_codex_hooks_json(&self, role: &str, hooks_source: &Path) -> Result<Value> {
        let hook_mapping: Vec<(&str, &str, &str, &str)> = match role {
            "forge" => vec![
                (
                    "SessionStart",
                    "session_start",
                    "Loading FORGE session context",
                    "forge",
                ),
                (
                    "PreToolUse",
                    "pre_bash_guard",
                    "Checking Bash command",
                    "forge",
                ),
                (
                    "PreToolUse",
                    "pre_write_check",
                    "Validating file write",
                    "forge",
                ),
                (
                    "PostToolUse",
                    "post_write_lint",
                    "Linting after changes",
                    "forge",
                ),
                (
                    "PreCompact",
                    "pre_compact_handoff",
                    "Preparing context reset",
                    "forge",
                ),
                (
                    "Stop",
                    "stop_require_artifact",
                    "Checking for required artifacts",
                    "forge",
                ),
                (
                    "SubagentStart",
                    "subagent_start",
                    "Initializing subagent",
                    "forge",
                ),
                (
                    "SubagentStop",
                    "subagent_stop",
                    "Validating subagent output",
                    "forge",
                ),
            ],
            "sentinel" => vec![
                (
                    "SessionStart",
                    "session_start",
                    "Loading SENTINEL session context",
                    "sentinel",
                ),
                (
                    "PreToolUse",
                    "pre_bash_readonly_guard",
                    "Enforcing read-only mode",
                    "sentinel",
                ),
                (
                    "PostToolUse",
                    "post_write_validate",
                    "Validating evaluation output",
                    "sentinel",
                ),
                (
                    "Stop",
                    "stop_require_eval",
                    "Checking for required evaluation",
                    "sentinel",
                ),
                (
                    "SubagentStart",
                    "subagent_start",
                    "Initializing subagent",
                    "sentinel",
                ),
                (
                    "SubagentStop",
                    "subagent_stop",
                    "Validating subagent evaluation",
                    "sentinel",
                ),
            ],
            _ => vec![],
        };

        let mut hooks_map = serde_json::Map::new();

        for (event, hook_name, status_msg, agent_dir) in &hook_mapping {
            let hook_script = hooks_source
                .join(agent_dir)
                .join(format!("{}.sh", hook_name));

            // Skip hooks that don't exist yet (e.g., subagent hooks)
            if !hook_script.exists() {
                debug!(
                    path = %hook_script.display(),
                    "Hook script not found, skipping"
                );
                continue;
            }

            // Reference the locally-installed copy via relative path from .codex/
            // (where hooks.json lives), making the harness portable across systems.
            let rel_path = format!("hooks/{}/{}.sh", agent_dir, hook_name);

            let hook_entry = json!({
                "matcher": match *event {
                    "PreToolUse" => {
                        match *hook_name {
                            "pre_bash_guard" | "pre_bash_readonly_guard" => "Bash",
                            "pre_write_check" => "apply_patch|Write",
                            _ => ".*"
                        }
                    },
                    "PostToolUse" => {
                        match *hook_name {
                            "post_write_lint" => "Bash",
                            "post_write_validate" => ".*",
                            _ => ".*"
                        }
                    },
                    _ => ".*"
                },
                "hooks": [{
                    "type": "command",
                    "command": rel_path,
                    "statusMessage": status_msg,
                }]
            });

            hooks_map
                .entry(*event)
                .or_insert_with(|| Value::Array(vec![]))
                .as_array_mut()
                .unwrap()
                .push(hook_entry);
        }

        Ok(json!({ "hooks": hooks_map }))
    }

    /// Generate Claude hooks configuration and install hook scripts.
    async fn generate_claude_hooks_json(
        &self,
        worktree: &Path,
        shared: &Path,
        _transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let hooks_source = self
            .orchestrator_dir()
            .join("orchestration")
            .join("plugin")
            .join("hooks");

        if !hooks_source.exists() {
            debug!("Hooks source directory not found, skipping Claude hooks generation");
            return Ok(());
        }

        // Install hook scripts for FORGE and update settings
        self.install_claude_hook_scripts(worktree, "forge", &hooks_source)?;
        self.add_hooks_to_claude_settings(worktree, "forge", &hooks_source)?;

        // Install hook scripts for SENTINEL and update settings
        self.install_claude_hook_scripts(shared, "sentinel", &hooks_source)?;
        self.add_hooks_to_claude_settings(shared, "sentinel", &hooks_source)?;

        info!("Claude hooks configuration generated for FORGE and SENTINEL");
        Ok(())
    }

    /// Copy hook scripts from source repo into .claude/hooks/{role}/ in the target directory.
    fn install_claude_hook_scripts(
        &self,
        target: &Path,
        role: &str,
        hooks_source: &Path,
    ) -> Result<()> {
        let hook_names: Vec<&str> = match role {
            "forge" => vec![
                "session_start",
                "pre_bash_guard",
                "pre_write_check",
                "post_write_lint",
                "pre_compact_handoff",
                "stop_require_artifact",
                "subagent_start",
                "subagent_stop",
            ],
            "sentinel" => vec![
                "session_start",
                "pre_bash_readonly_guard",
                "post_write_validate",
                "stop_require_eval",
                "subagent_start",
                "subagent_stop",
            ],
            _ => vec![],
        };

        let hooks_dest = target.join(".claude").join("hooks").join(role);
        fs::create_dir_all(&hooks_dest).context("Failed to create Claude hooks directory")?;

        for hook_name in &hook_names {
            let src = hooks_source.join(role).join(format!("{}.sh", hook_name));
            if !src.exists() {
                debug!(
                    path = %src.display(),
                    "Hook script not found in source, skipping copy"
                );
                continue;
            }
            let dst = hooks_dest.join(format!("{}.sh", hook_name));
            fs::copy(&src, &dst).context(format!("Failed to copy hook script {}", hook_name))?;

            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                let mode = fs::metadata(&dst)?.permissions().mode();
                if mode & 0o111 == 0 {
                    fs::set_permissions(&dst, fs::Permissions::from_mode(mode | 0o755))?;
                }
            }

            debug!(
                src = %src.display(),
                dst = %dst.display(),
                "Claude hook script copied"
            );
        }

        info!(
            path = %hooks_dest.display(),
            role = role,
            "Claude hook scripts installed"
        );
        Ok(())
    }

    /// Add hooks configuration to existing .claude/settings.json.
    fn add_hooks_to_claude_settings(
        &self,
        target: &Path,
        role: &str,
        hooks_source: &Path,
    ) -> Result<()> {
        let settings_path = target.join(".claude").join("settings.json");

        // Read existing settings
        let mut settings: Value = if settings_path.exists() {
            let content = fs::read_to_string(&settings_path)
                .context("Failed to read Claude settings.json")?;
            serde_json::from_str(&content).unwrap_or_else(|_| json!({}))
        } else {
            json!({})
        };

        let hook_mapping: Vec<(&str, &str, &str)> = match role {
            "forge" => vec![
                (
                    "SessionStart",
                    "session_start",
                    "Loading FORGE session context",
                ),
                ("PreToolUse", "pre_bash_guard", "Checking Bash command"),
                ("PreToolUse", "pre_write_check", "Validating file write"),
                ("PostToolUse", "post_write_lint", "Linting after changes"),
                (
                    "PreCompact",
                    "pre_compact_handoff",
                    "Preparing context reset",
                ),
                (
                    "Stop",
                    "stop_require_artifact",
                    "Checking for required artifacts",
                ),
                ("SubagentStart", "subagent_start", "Initializing subagent"),
                (
                    "SubagentStop",
                    "subagent_stop",
                    "Validating subagent output",
                ),
            ],
            "sentinel" => vec![
                (
                    "SessionStart",
                    "session_start",
                    "Loading SENTINEL session context",
                ),
                (
                    "PreToolUse",
                    "pre_bash_readonly_guard",
                    "Enforcing read-only mode",
                ),
                (
                    "PostToolUse",
                    "post_write_validate",
                    "Validating evaluation output",
                ),
                (
                    "Stop",
                    "stop_require_eval",
                    "Checking for required evaluation",
                ),
                ("SubagentStart", "subagent_start", "Initializing subagent"),
                (
                    "SubagentStop",
                    "subagent_stop",
                    "Validating subagent evaluation",
                ),
            ],
            _ => vec![],
        };

        let mut hooks_map = serde_json::Map::new();

        for (event, hook_name, status_msg) in &hook_mapping {
            let hook_script = hooks_source.join(role).join(format!("{}.sh", hook_name));
            if !hook_script.exists() {
                debug!(
                    path = %hook_script.display(),
                    "Hook script not found, skipping"
                );
                continue;
            }

            let rel_path = format!("hooks/{}/{}.sh", role, hook_name);

            let hook_entry = json!({
                "matcher": match *event {
                    "PreToolUse" => {
                        match *hook_name {
                            "pre_bash_guard" | "pre_bash_readonly_guard" => "Bash",
                            "pre_write_check" => "Write|Edit",
                            _ => ".*"
                        }
                    },
                    "PostToolUse" => {
                        match *hook_name {
                            "post_write_lint" => "Write|Edit",
                            "post_write_validate" => ".*",
                            _ => ".*"
                        }
                    },
                    _ => ".*"
                },
                "hooks": [{
                    "type": "command",
                    "command": rel_path,
                    "statusMessage": status_msg,
                }]
            });

            hooks_map
                .entry(*event)
                .or_insert_with(|| Value::Array(vec![]))
                .as_array_mut()
                .unwrap()
                .push(hook_entry);
        }

        if !hooks_map.is_empty() {
            settings["hooks"] = Value::Object(hooks_map);
        }

        self.write_json(&settings_path, &settings)?;
        info!(path = %settings_path.display(), role = role, "Claude hooks added to settings.json");
        Ok(())
    }

    /// Symlink (or copy in Coder mode) skills to .claude/skills/ in worktree.
    async fn symlink_skills_to_claude(
        &self,
        worktree: &Path,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let skills_source = self
            .orchestrator_dir()
            .join("orchestration")
            .join("plugin")
            .join("skills");

        if !skills_source.exists() {
            debug!("Skills source directory not found, skipping Claude skill symlinks");
            return Ok(());
        }

        let claude_skills_dir = worktree.join(".claude").join("skills");
        transport
            .create_dir_all(&claude_skills_dir.to_string_lossy())
            .await
            .context("Failed to create .claude/skills directory")?;

        for entry in std::fs::read_dir(&skills_source)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let skill_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if skill_name.is_empty() {
                continue;
            }

            let target_path = claude_skills_dir.join(skill_name);
            transport
                .symlink_or_copy(&path, &target_path.to_string_lossy())
                .await?;
        }

        info!(target = %claude_skills_dir.display(), "Skills symlinked to .claude/skills/");
        Ok(())
    }

    /// Symlink (or copy in Coder mode) role-relevant skills to .claude/skills/ in a target directory.
    async fn symlink_skills_to_claude_for_role(
        &self,
        target: &Path,
        role: &str,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let skills_source = self
            .orchestrator_dir()
            .join("orchestration")
            .join("plugin")
            .join("skills");

        if !skills_source.exists() {
            debug!("Skills source directory not found, skipping Claude role symlinks");
            return Ok(());
        }

        let claude_skills_dir = target.join(".claude").join("skills");
        transport
            .create_dir_all(&claude_skills_dir.to_string_lossy())
            .await
            .context("Failed to create .claude/skills directory")?;

        let prefix = format!("{}-", role);
        let shared_prefix = "shared-";

        for entry in std::fs::read_dir(&skills_source)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let skill_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if skill_name.is_empty() {
                continue;
            }

            if !skill_name.starts_with(&prefix) && !skill_name.starts_with(shared_prefix) {
                continue;
            }

            let target_path = claude_skills_dir.join(skill_name);
            transport
                .symlink_or_copy(&path, &target_path.to_string_lossy())
                .await?;
        }

        info!(
            target = %claude_skills_dir.display(),
            role = role,
            "Role-relevant skills symlinked to .claude/skills/"
        );
        Ok(())
    }

    /// Enhance Claude settings.json with permission rules.
    fn enhance_claude_permissions(&self, worktree: &Path, shared: &Path) -> Result<()> {
        // FORGE permissions
        let forge_settings = worktree.join(".claude").join("settings.json");
        if forge_settings.exists() {
            let content = fs::read_to_string(&forge_settings)
                .context("Failed to read FORGE settings.json")?;
            let mut settings: Value = serde_json::from_str(&content)
                .unwrap_or_else(|_| json!({ "permissions": { "defaultMode": "auto" } }));

            // Add permission rules for safety (informational when using --dangerously-skip-permissions)
            settings["permissions"]["allow"] = json!([
                { "tool": "Bash", "command": "cargo test" },
                { "tool": "Bash", "command": "cargo clippy" },
                { "tool": "Bash", "command": "npm test" },
                { "tool": "Bash", "command": "npx jest" },
                { "tool": "Bash", "command": "npx eslint" },
                { "tool": "Bash", "command": "ruff check" }
            ]);

            settings["permissions"]["deny"] = json!([
                { "tool": "Bash", "command": "rm -rf *" },
                { "tool": "Bash", "command": "sudo *" },
                { "tool": "Bash", "command": "git push *" },
                { "tool": "Bash", "command": "npm install *" },
                { "tool": "Bash", "command": "pip install *" },
                { "tool": "Bash", "command": "cargo install *" }
            ]);

            self.write_json(&forge_settings, &settings)?;
            info!(path = %forge_settings.display(), "Claude FORGE permissions enhanced");
        }

        // SENTINEL permissions (read-only)
        let sentinel_settings = shared.join(".claude").join("settings.json");
        if sentinel_settings.exists() {
            let content = fs::read_to_string(&sentinel_settings)
                .context("Failed to read SENTINEL settings.json")?;
            let mut settings: Value = serde_json::from_str(&content)
                .unwrap_or_else(|_| json!({ "permissions": { "defaultMode": "auto" } }));

            // SENTINEL is read-only, so deny all write operations
            settings["permissions"]["deny"] = json!([
                { "tool": "Write", "pattern": "*" },
                { "tool": "Edit", "pattern": "*" },
                { "tool": "Bash", "command": "git *" },
                { "tool": "Bash", "command": "rm *" },
                { "tool": "Bash", "command": "sudo *" },
                { "tool": "Bash", "command": "npm install *" },
                { "tool": "Bash", "command": "pip install *" }
            ]);

            self.write_json(&sentinel_settings, &settings)?;
            info!(path = %sentinel_settings.display(), "Claude SENTINEL permissions enhanced");
        }

        Ok(())
    }

    /// Symlink (or copy in Coder mode) skills to .agents/skills/ in worktree.
    async fn symlink_skills_to_agents(
        &self,
        worktree: &Path,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let skills_source = self
            .orchestrator_dir()
            .join("orchestration")
            .join("plugin")
            .join("skills");

        if !skills_source.exists() {
            debug!("Skills source directory not found, skipping symlinks");
            return Ok(());
        }

        let agents_skills_dir = worktree.join(".agents").join("skills");
        transport.create_dir_all(&agents_skills_dir.to_string_lossy()).await
            .context("Failed to create .agents/skills directory")?;

        for entry in std::fs::read_dir(&skills_source)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let skill_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if skill_name.is_empty() {
                continue;
            }

            let target_abs = agents_skills_dir.join(skill_name);
            transport
                .symlink_or_copy(&path, &target_abs.to_string_lossy())
                .await?;

            debug!(
                source = %path.display(),
                target = %target_abs.display(),
                "Skill symlinked/copied"
            );
        }

        info!(
            target = %agents_skills_dir.display(),
            "Skills symlinked to .agents/skills/"
        );
        Ok(())
    }

    /// Symlink (or copy in Coder mode) role-relevant skills to .agents/skills/ in a target directory.
    async fn symlink_skills_to_agents_for_role(
        &self,
        target: &Path,
        role: &str,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let skills_source = self
            .orchestrator_dir()
            .join("orchestration")
            .join("plugin")
            .join("skills");

        if !skills_source.exists() {
            debug!("Skills source directory not found, skipping role symlinks");
            return Ok(());
        }

        let agents_skills_dir = target.join(".agents").join("skills");
        transport.create_dir_all(&agents_skills_dir.to_string_lossy()).await
            .context("Failed to create .agents/skills directory")?;

        let prefix = format!("{}-", role);
        let shared_prefix = "shared-";

        for entry in std::fs::read_dir(&skills_source)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let skill_name = path.file_name().and_then(|n| n.to_str()).unwrap_or("");

            if skill_name.is_empty() {
                continue;
            }

            if !skill_name.starts_with(&prefix) && !skill_name.starts_with(shared_prefix) {
                continue;
            }

            let target_abs = agents_skills_dir.join(skill_name);
            transport
                .symlink_or_copy(&path, &target_abs.to_string_lossy())
                .await?;

            debug!(
                source = %path.display(),
                target = %target_abs.display(),
                role = role,
                "Role skill symlinked/copied"
            );
        }

        info!(
            target = %agents_skills_dir.display(),
            role = role,
            "Role-relevant skills symlinked to .agents/skills/"
        );
        Ok(())
    }

    /// Deploy the Codex plugin directory (.codex-plugin/) into the workspace.
    async fn deploy_codex_plugin(
        &self,
        target: &Path,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let plugin_source = self.orchestrator_dir().join("orchestration").join("plugin");

        let codex_plugin_source = plugin_source.join(".codex-plugin");

        if !codex_plugin_source.exists() {
            debug!(
                path = %codex_plugin_source.display(),
                "Codex plugin directory not found, skipping deployment"
            );
            return Ok(());
        }

        let plugins_dir = target.join(".agents").join("plugins");
        transport
            .create_dir_all(&plugins_dir.to_string_lossy())
            .await
            .context("Failed to create .agents/plugins directory")?;

        // Use transport's symlink_or_copy to handle Coder vs local workspaces
        let target_path = plugins_dir.join("orchestration");
        transport
            .symlink_or_copy(&plugin_source, &target_path.to_string_lossy())
            .await?;

        info!(
            source = %plugin_source.display(),
            target_dir = %plugins_dir.display(),
            "Codex plugin deployed"
        );
        Ok(())
    }

    /// Write AGENTS.md at worktree root from existing agent.md persona file.
    fn write_agents_md(&self, target: &Path, agent_id: &str) -> Result<()> {
        let agent_md_path = self
            .orchestrator_dir()
            .join("orchestration")
            .join("agent")
            .join("agents")
            .join(format!("{}.agent.md", agent_id));

        if !agent_md_path.exists() {
            debug!(
                path = %agent_md_path.display(),
                "Agent persona file not found, skipping AGENTS.md generation"
            );
            return Ok(());
        }

        let persona = fs::read_to_string(&agent_md_path)
            .context(format!("Failed to read {}", agent_md_path.display()))?;

        // Extract the body (after frontmatter) for AGENTS.md
        let body = if let Some(pos) = persona.find("\n---\n") {
            // Skip frontmatter
            let after_first_delim = &persona[pos + 5..];
            if let Some(pos2) = after_first_delim.find("\n---\n") {
                after_first_delim[pos2 + 5..].trim().to_string()
            } else {
                after_first_delim.trim().to_string()
            }
        } else {
            persona.clone()
        };

        let agents_md_path = target.join("AGENTS.md");

        let content = format!(
            "# {agent_id_uppercase} Agent Instructions\n\n\
            This file contains instructions for the {agent_id} agent in the AgentFlow orchestration system.\n\n\
            ---\n\n\
            {body}\n\n\
            ---\n\n\
            ## Working Agreements\n\n\
            - Always read standards files (CODING.md, SECURITY.md, REVIEW.md) before starting work\n\
            - Write STATUS.json with one of the valid status values when work is complete\n\
            - Push to remote after each commit\n\
            - Do NOT modify files outside your working directory\n\
            - Do NOT attempt to create PRs unless explicitly instructed\n",
            agent_id_uppercase = agent_id.to_uppercase(),
            agent_id = agent_id,
            body = body,
        );

        fs::write(&agents_md_path, content)
            .context(format!("Failed to write {}", agents_md_path.display()))?;
        info!(path = %agents_md_path.display(), "AGENTS.md generated");
        Ok(())
    }

    /// Generate Codex permission profiles (.codex/permissions.toml).
    async fn generate_codex_permissions(
        &self,
        target: &Path,
        shared: &Path,
        profile: &str,
        allowed_domains: &[String],
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let codex_dir = target.join(".codex");
        transport.create_dir_all(&codex_dir.to_string_lossy()).await.context("Failed to create .codex directory")?;

        let permissions_path = codex_dir.join("permissions.toml");

        let domain_lines: Vec<String> = if allowed_domains.is_empty() {
            vec![
                r#""api.github.com" = "allow""#.to_string(),
                r#""*.github.com" = "allow""#.to_string(),
            ]
        } else {
            allowed_domains
                .iter()
                .map(|d| format!(r#""{}" = "allow""#, d))
                .collect()
        };
        let domains_block = domain_lines.join("\n");

        let content = match profile {
            "workspace-write" => format!(
                r#"# Auto-generated by AgentFlow Provisioner
# FORGE/SENTINEL permissions: workspace-write with network access

default_permissions = "workspace-write"

[permissions.workspace-write.filesystem]
":minimal" = "read"

[permissions.workspace-write.filesystem.":workspace_roots"]
"." = "write"
"{shared}" = "write"
"**/*.env" = "deny"

[permissions.workspace-write.network]
enabled = true

[permissions.workspace-write.network.domains]
{domains}
"#,
                shared = shared.display(),
                domains = domains_block,
            ),
            "danger-full-access" => format!(
                r#"# Auto-generated by AgentFlow Provisioner
# FORGE permissions: full filesystem + restricted network
# FORGE needs git push and GitHub API access, plus any domains required
# by the project (package registries, API endpoints, etc.).
# Network is restricted to the allowlist below — no arbitrary access.

default_permissions = "danger-full-access"

[permissions.danger-full-access.filesystem]
":minimal" = "read"

[permissions.danger-full-access.filesystem.":workspace_roots"]
"." = "write"
"{shared}" = "write"
"**/*.env" = "deny"

[permissions.danger-full-access.network]
enabled = true

[permissions.danger-full-access.network.domains]
{domains}
"#,
                shared = shared.display(),
                domains = domains_block,
            ),
            "read-only" => r#"# Auto-generated by AgentFlow Provisioner
# SENTINEL permissions: read-only with GitHub API access

default_permissions = "read-only"

[permissions.read-only.filesystem]
":minimal" = "read"

[permissions.read-only.filesystem.":workspace_roots"]
"." = "read"

[permissions.read-only.network]
enabled = true

[permissions.read-only.network.domains]
"api.github.com" = "allow"
"*.github.com" = "allow"
"#
            .to_string(),
            _ => String::new(),
        };

        transport
            .write_file(&permissions_path.to_string_lossy(), &content)
            .await
            .context("Failed to write permissions.toml")?;
        info!(path = %permissions_path.display(), "Codex permissions profile generated");
        Ok(())
    }

    /// Write TICKET.md to shared directory.
    pub async fn write_ticket(
        &self,
        pair_id: &str,
        shared: &Path,
        ticket: &crate::types::Ticket,
        transport: &dyn WorkspaceTransport,
        pair_state: Option<&dyn PairStateStore>,
    ) -> Result<()> {
        let path = shared.join("TICKET.md");

        let content = format!(
            "# {}\n\n**Issue:** #{} \n**URL:** {}\n\n{}\n\n## Acceptance Criteria\n\n{}\n",
            ticket.title,
            ticket.issue_number,
            ticket.url,
            ticket.body,
            ticket
                .acceptance_criteria
                .iter()
                .map(|c| format!("- {}", c))
                .collect::<Vec<_>>()
                .join("\n")
        );

        transport
            .write_file(&path.to_string_lossy(), &content)
            .await
            .context("Failed to write TICKET.md")?;
        if let Some(store) = pair_state {
            store
                .write_artifact(pair_id, PairArtifact::Ticket, &content)
                .await?;
        }

        info!(path = %path.display(), "TICKET.md written");
        Ok(())
    }

    /// Write TASK.md to shared directory.
    pub async fn write_task(
        &self,
        pair_id: &str,
        shared: &Path,
        task: &str,
        transport: &dyn WorkspaceTransport,
        pair_state: Option<&dyn PairStateStore>,
    ) -> Result<()> {
        let path = shared.join("TASK.md");

        transport
            .write_file(&path.to_string_lossy(), task)
            .await
            .context("Failed to write TASK.md")?;
        if let Some(store) = pair_state {
            store
                .write_artifact(pair_id, PairArtifact::Task, task)
                .await?;
        }

        info!(path = %path.display(), "TASK.md written");
        Ok(())
    }

    /// Write standards files (CODING.md, SECURITY.md, REVIEW.md) into the workspace.
    ///
    /// These are read from the `orchestration/standards/` directory in the
    /// orchestrator source and provisioned into every workspace so agents have
    /// consistent style/code guidelines regardless of which repo they are placed in.
    ///
    /// In Coder mode, the files are written via the transport. In Local mode,
    /// they are written directly to the filesystem.
    pub async fn write_standards_files(
        &self,
        worktree: &Path,
        transport: &dyn WorkspaceTransport,
    ) -> Result<()> {
        let standards_dir = self.orchestrator_dir().join("orchestration").join("standards");
        let standard_files = ["CODING.md", "SECURITY.md", "REVIEW.md"];

        for file_name in &standard_files {
            let source_path = standards_dir.join(file_name);
            if !source_path.exists() {
                debug!(
                    path = %source_path.display(),
                    "Standards file not found, skipping",
                );
                continue;
            }

            let content = fs::read_to_string(&source_path)
                .context(format!("Failed to read standards file {}", source_path.display()))?;

            let target_path = worktree.join(file_name);
            transport
                .write_file(&target_path.to_string_lossy(), &content)
                .await
                .context(format!("Failed to write {}", target_path.display()))?;

            info!(path = %target_path.display(), "Standards file provisioned");
        }

        Ok(())
    }
}

fn shell_quote(path: &Path) -> String {
    let s = path.to_string_lossy();
    format!("'{}'", s.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::transport::LocalTransport;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_create_forge_settings() {
        let dir = tempdir().unwrap();
        let worktree = dir.path();
        let shared = dir.path().join("shared");
        let backend_config = BackendConfig::claude("", worktree, &shared);

        let provisioner = Provisioner::new(dir.path());
        let transport = LocalTransport::new(dir.path());
        provisioner
            .create_forge_settings(worktree, &backend_config, &transport)
            .await
            .unwrap();

        let settings_path = worktree.join(".claude").join("settings.json");
        assert!(settings_path.exists());

        let content = fs::read_to_string(&settings_path).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(settings["permissions"]["defaultMode"], "auto");
    }

    #[tokio::test]
    async fn test_create_sentinel_settings() {
        let dir = tempdir().unwrap();
        let shared = dir.path();
        let worktree = dir.path().join("worktree");
        let backend_config = BackendConfig::claude("", &worktree, shared);

        let provisioner = Provisioner::new(dir.path());
        let transport = LocalTransport::new(dir.path());
        provisioner
            .create_sentinel_settings(shared, &backend_config, &transport)
            .await
            .unwrap();

        let settings_path = shared.join(".claude").join("settings.json");
        assert!(settings_path.exists());
        assert!(!shared.join("sentinel").exists());

        let content = fs::read_to_string(&settings_path).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(settings["permissions"]["defaultMode"], "auto");
    }

    #[tokio::test]
    async fn test_create_shared_structure() {
        let dir = tempdir().unwrap();
        let shared = dir.path().join("shared");

        let provisioner = Provisioner::new(dir.path());
        let transport = LocalTransport::new(dir.path());
        provisioner
            .create_shared_structure(&shared, &transport)
            .await
            .unwrap();

        assert!(shared.exists());
        assert!(!shared.join("sentinel").exists());
        assert!(shared.join(".gitignore").exists());
    }

    #[tokio::test]
    async fn test_install_hook_scripts_copies_files() {
        let dir = tempdir().unwrap();
        let target = dir.path().join("target");
        fs::create_dir_all(&target).unwrap();

        // Create fake source hooks directory with sentinel scripts
        let hooks_source = dir.path().join("hooks");
        let sentinel_src = hooks_source.join("sentinel");
        fs::create_dir_all(&sentinel_src).unwrap();
        fs::write(
            sentinel_src.join("session_start.sh"),
            "#!/bin/bash\necho sentinel-start",
        )
        .unwrap();

        let provisioner = Provisioner::new(dir.path());
        let transport = LocalTransport::new(dir.path());
        provisioner
            .install_hook_scripts(&target, "sentinel", &hooks_source, &transport)
            .await
            .unwrap();

        // Verify the script was copied to .codex/hooks/sentinel/
        let copied = target
            .join(".codex")
            .join("hooks")
            .join("sentinel")
            .join("session_start.sh");
        assert!(
            copied.exists(),
            "Hook script should be copied to .codex/hooks/sentinel/"
        );

        let content = fs::read_to_string(&copied).unwrap();
        assert_eq!(content, "#!/bin/bash\necho sentinel-start");
    }
}
