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

use crate::process::{get_backend_config, BackendConfig};
use crate::types::CliBackend;

/// Provisions configuration files for pairs.
pub struct Provisioner {
    /// Project root directory
    project_root: PathBuf,
}

impl Provisioner {
    /// Create a new provisioner.
    pub fn new(project_root: impl Into<PathBuf>) -> Self {
        Self {
            project_root: project_root.into(),
        }
    }

    /// Provision all configuration for a pair using BackendConfig.
    pub async fn provision_pair(
        &self,
        pair_id: &str,
        worktree: &Path,
        shared: &Path,
        github_token: &str,
        redis_url: Option<&str>,
        cli_backend: CliBackend,
    ) -> Result<()> {
        info!(pair = pair_id, backend = ?cli_backend, "Provisioning pair configuration");

        let backend_config = get_backend_config(cli_backend, worktree, shared);

        // 1. Create FORGE settings/config
        self.create_forge_settings(worktree, &backend_config)?;

        // 2. Create SENTINEL settings/config
        self.create_sentinel_settings(shared, &backend_config)?;

        // 3. Create FORGE mcp.json (if backend uses MCP config)
        if !backend_config.mcp_config_rel.as_os_str().is_empty() {
            let mcp_gen = crate::mcp_config::McpConfigGenerator::new(github_token, redis_url);
            let mcp_path = backend_config.mcp_config_path(worktree);
            mcp_gen.generate_forge_config(worktree, shared, &mcp_path)?;
        }

        // 4. Create SENTINEL mcp.json
        if !backend_config.mcp_config_rel.as_os_str().is_empty() {
            let mcp_gen = crate::mcp_config::McpConfigGenerator::new(github_token, redis_url);
            let mcp_path = backend_config.mcp_config_path(shared);
            mcp_gen.generate_sentinel_config(worktree, shared, &mcp_path)?;
        }

        // 5. Symlink plugin to FORGE
        self.symlink_plugin(worktree, "forge")?;

        // 6. Symlink plugin to SENTINEL
        self.symlink_plugin(shared, "sentinel")?;

        // 7. Create shared directory structure
        self.create_shared_structure(shared)?;

        // 8. Backend-specific provisioning (Codex: agent TOMLs, hooks, permissions, etc.)
        if backend_config.needs_extras_provisioning {
            self.provision_codex_extras(worktree, shared, github_token, redis_url)?;
        }

        info!(pair = pair_id, backend = ?cli_backend, "Pair provisioning complete");
        Ok(())
    }

    /// Provision Codex-native extras (.codex/, .agents/, AGENTS.md).
    fn provision_codex_extras(
        &self,
        worktree: &Path,
        shared: &Path,
        github_token: &str,
        redis_url: Option<&str>,
    ) -> Result<()> {
        // 1. Generate .codex/config.toml for FORGE worktree
        self.generate_codex_config_toml(worktree, github_token, redis_url, "workspace-write")?;

        // 2. Generate .codex/config.toml for SENTINEL shared dir
        self.generate_codex_config_toml(shared, github_token, redis_url, "read-only")?;

        // 3. Generate .codex/agents/*.toml from existing agent.md files
        self.generate_codex_agent_tomls(worktree)?;

        // 4. Generate .codex/hooks.json with absolute paths
        self.generate_codex_hooks_json(worktree, shared)?;

        // 5. Symlink skills to .agents/skills/ in worktree
        self.symlink_skills_to_agents(worktree)?;

        // 6. Write AGENTS.md at worktree root from forge.agent.md
        self.write_agents_md(worktree, "forge")?;

        // 7. Write AGENTS.md at shared root from sentinel.agent.md
        self.write_agents_md(shared, "sentinel")?;

        // 8. Generate Codex permission profiles
        self.generate_codex_permissions(worktree, "workspace-write")?;
        self.generate_codex_permissions(shared, "read-only")?;

        Ok(())
    }

    /// Create FORGE's settings.json with auto-mode permissions.
    pub fn create_forge_settings(&self, worktree: &Path, config: &BackendConfig) -> Result<()> {
        let settings_dir = worktree.join(&config.settings_rel.parent().unwrap_or(&config.settings_rel));
        fs::create_dir_all(&settings_dir).context("Failed to create settings directory")?;

        let settings_path = config.settings_path(worktree);

        info!(path = %settings_path.display(), "Creating FORGE settings");

        // Minimal settings - permissions are handled by --dangerously-skip-permissions flag
        let settings = json!({
            "permissions": {
                "defaultMode": "auto"
            }
        });

        self.write_json(&settings_path, &settings)?;

        self.ensure_worktree_gitignore(worktree, config)
    }

    fn ensure_worktree_gitignore(&self, worktree: &Path, config: &BackendConfig) -> Result<()> {
        let gitignore_path = worktree.join(".gitignore");
        let settings_dir_name = config.settings_rel.parent()
            .and_then(|p| p.file_name())
            .map(|n| n.to_string_lossy().to_string())
            .unwrap_or_else(|| ".claude/".to_string());
        let settings_entry = format!("{}/", settings_dir_name);

        let existing = fs::read_to_string(&gitignore_path).unwrap_or_default();

        if !existing.lines().any(|l| l.trim() == settings_entry) {
            let updated = if existing.is_empty() {
                format!("{}\n", settings_entry)
            } else if existing.ends_with('\n') {
                format!("{}{}\n", existing, settings_entry)
            } else {
                format!("{}\n{}\n", existing, settings_entry)
            };
            fs::write(&gitignore_path, updated)
                .context("Failed to update .gitignore with settings directory exclusion")?;
            info!(
                path = %gitignore_path.display(),
                "Added {} to worktree .gitignore", settings_entry
            );
        }

        Ok(())
    }

    /// Create SENTINEL's settings.json with read-only permissions.
    pub fn create_sentinel_settings(&self, shared: &Path, config: &BackendConfig) -> Result<()> {
        let legacy_dir = shared.join("sentinel");
        if legacy_dir.exists() {
            fs::remove_dir_all(&legacy_dir)
                .context("Failed to remove legacy sentinel directory")?;
        }

        let settings_dir = shared.join(&config.settings_rel.parent().unwrap_or(&config.settings_rel));
        fs::create_dir_all(&settings_dir).context("Failed to create sentinel settings directory")?;

        let settings_path = config.settings_path(shared);

        info!(path = %settings_path.display(), "Creating SENTINEL settings");

        // Minimal settings - permissions are handled by --dangerously-skip-permissions flag
        let settings = json!({
            "permissions": {
                "defaultMode": "auto"
            }
        });

        self.write_json(&settings_path, &settings)
    }

    /// Symlink the Sprintless plugin to a .claude directory.
    pub fn symlink_plugin(&self, target_dir: &Path, role: &str) -> Result<()> {
        // First check for ORCHESTRATOR_DIR env var (points to orchestrator source with plugin)
        // Fall back to project_root for backwards compatibility
        let plugin_source = if let Ok(orch_dir) = std::env::var("ORCHESTRATOR_DIR") {
            PathBuf::from(orch_dir).join("orchestration").join("plugin")
        } else {
            self.project_root.join("orchestration").join("plugin")
        };

        // Check if plugin exists
        if !plugin_source.exists() {
            debug!(
                role = role,
                path = %plugin_source.display(),
                "Plugin directory not found, skipping symlink"
            );
            return Ok(());
        }

        let plugins_dir = target_dir.join(".claude").join("plugins");

        fs::create_dir_all(&plugins_dir).context("Failed to create plugins directory")?;

        let symlink_path = plugins_dir.join("orchestration");

        // Remove existing symlink if present
        if symlink_path.exists() || symlink_path.symlink_metadata().is_ok() {
            let _ = fs::remove_file(&symlink_path);
        }

        // Create symlink
        #[cfg(unix)]
        std::os::unix::fs::symlink(&plugin_source, &symlink_path)
            .context("Failed to create plugin symlink")?;

        #[cfg(windows)]
        std::os::windows::fs::symlink_dir(&plugin_source, &symlink_path)
            .context("Failed to create plugin symlink")?;

        debug!(
            role = role,
            source = %plugin_source.display(),
            target = %symlink_path.display(),
            "Plugin symlinked"
        );

        Ok(())
    }

    /// Create the shared directory structure.
    pub fn create_shared_structure(&self, shared: &Path) -> Result<()> {
        let already_exists = shared.exists();

        fs::create_dir_all(shared).context("Failed to create shared directory")?;

        // Clean up the legacy sentinel subdirectory from older runs.
        let legacy_dir = shared.join("sentinel");
        if legacy_dir.exists() {
            fs::remove_dir_all(&legacy_dir)
                .context("Failed to remove legacy sentinel directory")?;
        }

        // Create .gitignore for shared directory
        let gitignore = shared.join(".gitignore");
        fs::write(
            &gitignore,
            "# Shared artifacts are runtime state, not committed\n*\n!.gitignore\n",
        )
        .context("Failed to write .gitignore")?;

        // On re-provision (e.g. CI fix, conflict rework), write a fresh WORKLOG.md
        // so the watchdog doesn't see a stale mtime from a previous lifecycle and
        // immediately declare the pair stalled.
        if already_exists {
            let worklog_path = shared.join("WORKLOG.md");
            fs::write(&worklog_path, "# Worklog\n\n")
                .context("Failed to reset WORKLOG.md on re-provision")?;
            debug!(path = %worklog_path.display(), "Reset WORKLOG.md for re-provisioned pair");
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

    /// Generate .codex/config.toml for a given directory.
    fn generate_codex_config_toml(
        &self,
        target: &Path,
        github_token: &str,
        redis_url: Option<&str>,
        sandbox_mode: &str,
    ) -> Result<()> {
        let codex_dir = target.join(".codex");
        fs::create_dir_all(&codex_dir).context("Failed to create .codex directory")?;

        let config_path = codex_dir.join("config.toml");

        let network_access = sandbox_mode == "workspace-write";
        let approval_policy = if sandbox_mode == "read-only" {
            "always"
        } else {
            "on-request"
        };

        let _redis_url_val = redis_url.unwrap_or("");
        let mcp_shell_args = if sandbox_mode == "workspace-write" {
            r#""orchestration/agent/tooling/run-tests.sh,cargo clippy,cargo test,npx eslint,npx jest,ruff check""#
        } else {
            r#""orchestration/agent/tooling/run-tests.sh,npx eslint,ruff check,cargo clippy""#
        };

        let config = format!(
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
            worktree = target.display(),
            shared = target.display(),
        );

        fs::write(&config_path, config).context("Failed to write .codex/config.toml")?;
        info!(path = %config_path.display(), "Codex config.toml generated");
        Ok(())
    }

    /// Generate .codex/agents/*.toml from existing agent.md files.
    fn generate_codex_agent_tomls(&self, worktree: &Path) -> Result<()> {
        let agents_dir = worktree.join(".codex").join("agents");
        fs::create_dir_all(&agents_dir).context("Failed to create .codex/agents directory")?;

        let agent_ids = ["forge", "sentinel"];

        for agent_id in &agent_ids {
            let agent_md_path = self
                .project_root
                .join("orchestration")
                .join("agent")
                .join("agents")
                .join(format!("{}.agent.md", agent_id));

            if !agent_md_path.exists() {
                debug!(
                    path = %agent_md_path.display(),
                    "Agent persona file not found, skipping TOML generation"
                );
                continue;
            }

            let persona = fs::read_to_string(&agent_md_path)
                .context(format!("Failed to read {}", agent_md_path.display()))?;

            let (role, model, sandbox_mode) = match *agent_id {
                "forge" => (
                    "builder",
                    "gpt-5.4",
                    "workspace-write",
                ),
                "sentinel" => (
                    "reviewer",
                    "gpt-5.4",
                    "read-only",
                ),
                _ => ("unknown", "gpt-5.4", "workspace-write"),
            };

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
            fs::write(&toml_path, toml_content)
                .context(format!("Failed to write {}", toml_path.display()))?;
            info!(path = %toml_path.display(), "Codex agent TOML generated");
        }

        Ok(())
    }

    /// Generate .codex/hooks.json with absolute paths to hook scripts.
    fn generate_codex_hooks_json(&self, worktree: &Path, shared: &Path) -> Result<()> {
        let hooks_source = self
            .project_root
            .join("orchestration")
            .join("plugin")
            .join("hooks");

        if !hooks_source.exists() {
            debug!("Hooks source directory not found, skipping hooks.json generation");
            return Ok(());
        }

        // Generate FORGE hooks
        let forge_hooks = self.build_codex_hooks_json("forge", &hooks_source)?;
        let forge_hooks_path = worktree.join(".codex").join("hooks.json");
        fs::create_dir_all(forge_hooks_path.parent().unwrap())?;
        self.write_json(&forge_hooks_path, &forge_hooks)?;
        info!(path = %forge_hooks_path.display(), "Codex hooks.json generated for FORGE");

        // Generate SENTINEL hooks
        let sentinel_hooks = self.build_codex_hooks_json("sentinel", &hooks_source)?;
        let sentinel_hooks_path = shared.join(".codex").join("hooks.json");
        fs::create_dir_all(sentinel_hooks_path.parent().unwrap())?;
        self.write_json(&sentinel_hooks_path, &sentinel_hooks)?;
        info!(path = %sentinel_hooks_path.display(), "Codex hooks.json generated for SENTINEL");

        Ok(())
    }

    /// Build the Codex hooks.json structure for a given agent role.
    fn build_codex_hooks_json(&self, role: &str, hooks_source: &Path) -> Result<Value> {
        let hook_mapping: Vec<(&str, &str, &str, &str)> = match role {
            "forge" => vec![
                ("SessionStart", "session_start", "Loading FORGE session context", "forge"),
                ("PreToolUse", "pre_bash_guard", "Checking Bash command", "forge"),
                ("PreToolUse", "pre_write_check", "Validating file write", "forge"),
                ("PostToolUse", "post_write_lint", "Linting after changes", "forge"),
                ("PreCompact", "pre_compact_handoff", "Preparing context reset", "forge"),
                ("Stop", "stop_require_artifact", "Checking for required artifacts", "forge"),
                ("SubagentStart", "subagent_start", "Initializing subagent", "forge"),
                ("SubagentStop", "subagent_stop", "Validating subagent output", "forge"),
            ],
            "sentinel" => vec![
                ("SessionStart", "session_start", "Loading SENTINEL session context", "sentinel"),
                ("PreToolUse", "pre_bash_readonly_guard", "Enforcing read-only mode", "sentinel"),
                ("PostToolUse", "post_write_validate", "Validating evaluation output", "sentinel"),
                ("Stop", "stop_require_eval", "Checking for required evaluation", "sentinel"),
                ("SubagentStart", "subagent_start", "Initializing subagent", "sentinel"),
                ("SubagentStop", "subagent_stop", "Validating subagent evaluation", "sentinel"),
            ],
            _ => vec![],
        };

        let mut hooks_map = serde_json::Map::new();

        for (event, hook_name, status_msg, agent_dir) in &hook_mapping {
            let hook_script = hooks_source.join(agent_dir).join(format!("{}.sh", hook_name));

            // Skip hooks that don't exist yet (e.g., subagent hooks)
            if !hook_script.exists() {
                debug!(
                    path = %hook_script.display(),
                    "Hook script not found, skipping"
                );
                continue;
            }

            let abs_path = hook_script.canonicalize().unwrap_or_else(|_| hook_script.clone());

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
                    "command": abs_path.to_string_lossy().to_string(),
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

    /// Symlink skills to .agents/skills/ in worktree.
    fn symlink_skills_to_agents(&self, worktree: &Path) -> Result<()> {
        let skills_source = self.project_root.join("orchestration").join("plugin").join("skills");

        if !skills_source.exists() {
            debug!("Skills source directory not found, skipping symlinks");
            return Ok(());
        }

        let agents_skills_dir = worktree.join(".agents").join("skills");
        fs::create_dir_all(&agents_skills_dir).context("Failed to create .agents/skills directory")?;

        // Find all SKILL.md directories and symlink them
        for entry in fs::read_dir(&skills_source)? {
            let entry = entry?;
            let path = entry.path();

            if !path.is_dir() {
                continue;
            }

            let skill_name = path
                .file_name()
                .and_then(|n| n.to_str())
                .unwrap_or("");

            if skill_name.is_empty() {
                continue;
            }

            let symlink_path = agents_skills_dir.join(skill_name);

            // Remove existing symlink if present
            if symlink_path.exists() || symlink_path.symlink_metadata().is_ok() {
                let _ = fs::remove_file(&symlink_path);
            }

            // Create symlink
            #[cfg(unix)]
            {
                std::os::unix::fs::symlink(&path, &symlink_path)
                    .context(format!("Failed to symlink skill {}", skill_name))?;
            }

            #[cfg(windows)]
            {
                std::os::windows::fs::symlink_dir(&path, &symlink_path)
                    .context(format!("Failed to symlink skill {}", skill_name))?;
            }

            debug!(
                source = %path.display(),
                target = %symlink_path.display(),
                "Skill symlinked"
            );
        }

        info!(
            target = %agents_skills_dir.display(),
            "Skills symlinked to .agents/skills/"
        );
        Ok(())
    }

    /// Write AGENTS.md at worktree root from existing agent.md persona file.
    fn write_agents_md(&self, target: &Path, agent_id: &str) -> Result<()> {
        let agent_md_path = self
            .project_root
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
    fn generate_codex_permissions(&self, target: &Path, profile: &str) -> Result<()> {
        let codex_dir = target.join(".codex");
        fs::create_dir_all(&codex_dir)?;

        let permissions_path = codex_dir.join("permissions.toml");

        let content = match profile {
            "workspace-write" => r#"# Auto-generated by AgentFlow Provisioner
# FORGE permissions: workspace-write with network access

default_permissions = "workspace-write"

[permissions.workspace-write.filesystem]
":minimal" = "read"

[permissions.workspace-write.filesystem.":workspace_roots"]
"." = "write"
"**/*.env" = "deny"

[permissions.workspace-write.network]
enabled = true

[permissions.workspace-write.network.domains]
"api.github.com" = "allow"
"*.github.com" = "allow"
"#,
            "read-only" => r#"# Auto-generated by AgentFlow Provisioner
# SENTINEL permissions: read-only

default_permissions = "read-only"

[permissions.read-only.filesystem]
":minimal" = "read"

[permissions.read-only.filesystem.":workspace_roots"]
"." = "read"

[permissions.read-only.network]
enabled = false
"#,
            _ => "",
        };

        fs::write(&permissions_path, content)
            .context("Failed to write permissions.toml")?;
        info!(path = %permissions_path.display(), "Codex permissions profile generated");
        Ok(())
    }

    /// Write TICKET.md to shared directory.
    pub fn write_ticket(&self, shared: &Path, ticket: &crate::types::Ticket) -> Result<()> {
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

        fs::write(&path, content).context("Failed to write TICKET.md")?;

        info!(path = %path.display(), "TICKET.md written");
        Ok(())
    }

    /// Write TASK.md to shared directory.
    pub fn write_task(&self, shared: &Path, task: &str) -> Result<()> {
        let path = shared.join("TASK.md");

        fs::write(&path, task).context("Failed to write TASK.md")?;

        info!(path = %path.display(), "TASK.md written");
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn test_create_forge_settings() {
        let dir = tempdir().unwrap();
        let worktree = dir.path();
        let shared = dir.path().join("shared");
        let backend_config = BackendConfig::claude("", worktree, &shared);

        let provisioner = Provisioner::new(dir.path());
        provisioner.create_forge_settings(worktree, &backend_config).unwrap();

        let settings_path = worktree.join(".claude").join("settings.json");
        assert!(settings_path.exists());

        let content = fs::read_to_string(&settings_path).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(settings["permissions"]["defaultMode"], "auto");
    }

    #[test]
    fn test_create_sentinel_settings() {
        let dir = tempdir().unwrap();
        let shared = dir.path();
        let worktree = dir.path().join("worktree");
        let backend_config = BackendConfig::claude("", &worktree, shared);

        let provisioner = Provisioner::new(dir.path());
        provisioner.create_sentinel_settings(shared, &backend_config).unwrap();

        let settings_path = shared.join(".claude").join("settings.json");
        assert!(settings_path.exists());
        assert!(!shared.join("sentinel").exists());

        let content = fs::read_to_string(&settings_path).unwrap();
        let settings: Value = serde_json::from_str(&content).unwrap();

        assert_eq!(settings["permissions"]["defaultMode"], "auto");
    }

    #[test]
    fn test_create_shared_structure() {
        let dir = tempdir().unwrap();
        let shared = dir.path().join("shared");

        let provisioner = Provisioner::new(dir.path());
        provisioner.create_shared_structure(&shared).unwrap();

        assert!(shared.exists());
        assert!(!shared.join("sentinel").exists());
        assert!(shared.join(".gitignore").exists());
    }
}
