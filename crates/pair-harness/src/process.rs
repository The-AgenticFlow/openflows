// crates/pair-harness/src/process.rs
//! Process management for FORGE and SENTINEL agents.
//!
//! Supports multiple CLI backends:
//! - Claude Code CLI (default): `claude --print --dangerously-skip-permissions --output-format stream-json`
//! - OpenAI Codex CLI: `codex exec --full-auto --dangerously-bypass-approvals-and-sandbox "<prompt>"`

use anyhow::{anyhow, Context, Result};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, error, info, warn};

use crate::types::CliBackend;

#[cfg(unix)]
fn is_executable(path: &Path) -> bool {
    use std::os::unix::fs::MetadataExt;
    path.metadata()
        .map(|m| m.mode() & 0o111 != 0)
        .unwrap_or(false)
}

#[cfg(not(unix))]
fn is_executable(_path: &Path) -> bool {
    true
}

/// Mode for SENTINEL spawning.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SentinelMode {
    /// Plan review mode (SPRINTLESS_SEGMENT is empty)
    PlanReview,
    /// Segment evaluation mode (SPRINTLESS_SEGMENT is set)
    SegmentEval(u32),
    /// Final review mode
    FinalReview,
}

impl SentinelMode {
    /// Get the SPRINTLESS_SEGMENT value for this mode.
    pub fn segment_value(&self) -> String {
        match self {
            SentinelMode::PlanReview => String::new(),
            SentinelMode::SegmentEval(n) => n.to_string(),
            SentinelMode::FinalReview => "final".to_string(),
        }
    }
}

/// Manages FORGE and SENTINEL processes.
/// Supports both Claude Code and Codex CLI backends.
pub struct ProcessManager {
    /// Path to Claude CLI binary
    claude_path: PathBuf,
    /// Path to Codex CLI binary
    codex_path: PathBuf,
    /// Default CLI backend to use
    default_backend: CliBackend,
    github_token: String,
    redis_url: Option<String>,
    proxy_url: Option<String>,
    proxy_api_key: Option<String>,
}

impl ProcessManager {
    /// Create a new ProcessManager with default CLI backend (Claude).
    pub fn new(github_token: impl Into<String>) -> Self {
        let claude_path = std::env::var("CLAUDE_PATH").unwrap_or_else(|_| "claude".to_string());
        let claude_path = PathBuf::from(&claude_path);
        let codex_path = std::env::var("CODEX_PATH").unwrap_or_else(|_| "codex".to_string());
        let codex_path = PathBuf::from(&codex_path);

        Self::validate_cli_binary(&claude_path, "claude");
        Self::validate_cli_binary(&codex_path, "codex");

        let proxy_url = std::env::var("PROXY_URL").ok();
        let proxy_api_key = std::env::var("PROXY_API_KEY").ok();

        Self {
            claude_path,
            codex_path,
            default_backend: CliBackend::default(),
            github_token: github_token.into(),
            redis_url: None,
            proxy_url,
            proxy_api_key,
        }
    }

    /// Create a ProcessManager with Redis backend.
    pub fn with_redis(github_token: impl Into<String>, redis_url: impl Into<String>) -> Self {
        let claude_path = std::env::var("CLAUDE_PATH").unwrap_or_else(|_| "claude".to_string());
        let claude_path = PathBuf::from(&claude_path);
        let codex_path = std::env::var("CODEX_PATH").unwrap_or_else(|_| "codex".to_string());
        let codex_path = PathBuf::from(&codex_path);

        Self::validate_cli_binary(&claude_path, "claude");
        Self::validate_cli_binary(&codex_path, "codex");

        let proxy_url = std::env::var("PROXY_URL").ok();
        let proxy_api_key = std::env::var("PROXY_API_KEY").ok();

        Self {
            claude_path,
            codex_path,
            default_backend: CliBackend::default(),
            github_token: github_token.into(),
            redis_url: Some(redis_url.into()),
            proxy_url,
            proxy_api_key,
        }
    }

    /// Create a ProcessManager with proxy configuration.
    pub fn with_proxy(
        github_token: impl Into<String>,
        redis_url: Option<String>,
        proxy_url: impl Into<String>,
    ) -> Self {
        let claude_path = std::env::var("CLAUDE_PATH").unwrap_or_else(|_| "claude".to_string());
        let claude_path = PathBuf::from(&claude_path);
        let codex_path = std::env::var("CODEX_PATH").unwrap_or_else(|_| "codex".to_string());
        let codex_path = PathBuf::from(&codex_path);

        Self::validate_cli_binary(&claude_path, "claude");
        Self::validate_cli_binary(&codex_path, "codex");

        let proxy_api_key = std::env::var("PROXY_API_KEY").ok();

        Self {
            claude_path,
            codex_path,
            default_backend: CliBackend::default(),
            github_token: github_token.into(),
            redis_url,
            proxy_url: Some(proxy_url.into()),
            proxy_api_key,
        }
    }

    /// Set the default CLI backend.
    pub fn with_default_backend(mut self, backend: CliBackend) -> Self {
        self.default_backend = backend;
        self
    }

    /// Validate a CLI binary exists and is executable.
    fn validate_cli_binary(path: &Path, name: &str) {
        let env_var = format!("{}_PATH", name.to_uppercase());
        if path.is_absolute() {
            if !path.exists() {
                error!(
                    path = %path.display(),
                    "{} binary not found. Install {} CLI or set {} in .env",
                    env_var, name, env_var
                );
            } else if !is_executable(path) {
                error!(
                    path = %path.display(),
                    "{} binary exists but is not executable. Run: chmod +x {}",
                    env_var, path.display()
                );
            }
        } else {
            match which::which(path) {
                Ok(found) => {
                    debug!(path = %found.display(), "{} CLI binary found", name);
                }
                Err(_) => {
                    let install_url = match name {
                        "claude" => "https://claude.ai/download",
                        "codex" => "https://github.com/openai/codex",
                        _ => "the vendor's website",
                    };
                    error!(
                        binary = %path.display(),
                        "{} CLI binary not found on PATH. Install it from {} or set {}_PATH in .env to an absolute path",
                        name, install_url, name.to_uppercase()
                    );
                }
            }
        }
    }

    fn inject_proxy_env(
        cmd: &mut Command,
        routing_key: &str,
        proxy_url: &str,
        proxy_api_key: Option<&str>,
    ) {
        let base_url = proxy_url.trim_end_matches("/v1").trim_end_matches('/');
        cmd.env("ANTHROPIC_BASE_URL", base_url);
        if let Some(api_key) = proxy_api_key {
            cmd.env("ANTHROPIC_API_KEY", api_key);
        } else {
            cmd.env("ANTHROPIC_API_KEY", routing_key);
        }
    }

    fn inject_llm_env(cmd: &mut Command) {
        cmd.env(
            "LLM_PROVIDER",
            std::env::var("LLM_PROVIDER").unwrap_or_else(|_| "fallback".to_string()),
        );
        cmd.env(
            "LLM_FALLBACK",
            std::env::var("LLM_FALLBACK").unwrap_or_default(),
        );
        cmd.env(
            "MODEL_PROVIDER_MAP",
            std::env::var("MODEL_PROVIDER_MAP").unwrap_or_default(),
        );
        cmd.env(
            "ANTHROPIC_MODEL",
            std::env::var("ANTHROPIC_MODEL").unwrap_or_default(),
        );
        cmd.env(
            "OPENAI_API_KEY",
            std::env::var("OPENAI_API_KEY").unwrap_or_default(),
        );
        cmd.env(
            "OPENAI_MODEL",
            std::env::var("OPENAI_MODEL").unwrap_or_default(),
        );
        cmd.env(
            "GEMINI_API_KEY",
            std::env::var("GEMINI_API_KEY").unwrap_or_default(),
        );
        cmd.env(
            "GEMINI_MODEL",
            std::env::var("GEMINI_MODEL").unwrap_or_default(),
        );
    }

    pub fn proxy_url(&self) -> Option<&str> {
        self.proxy_url.as_deref()
    }

    pub fn proxy_api_key(&self) -> Option<&str> {
        self.proxy_api_key.as_deref()
    }

    /// Get the CLI binary path for a given backend.
    fn get_cli_path(&self, backend: CliBackend) -> &Path {
        match backend {
            CliBackend::Claude => &self.claude_path,
            CliBackend::Codex => &self.codex_path,
        }
    }

    /// Build a command for the appropriate CLI backend.
    fn build_cli_command(&self, backend: CliBackend, _worktree: &Path, _shared: &Path) -> Command {
        let cli_path = self.get_cli_path(backend);
        let mut cmd = Command::new(cli_path);

        match backend {
            CliBackend::Claude => {
                // Claude Code CLI flags
                cmd.arg("--print")
                    .arg("--dangerously-skip-permissions")
                    .arg("--output-format")
                    .arg("stream-json")
                    .arg("--verbose");
            }
            CliBackend::Codex => {
                // Codex CLI flags - use 'exec' subcommand for non-interactive execution
                // --full-auto enables fully autonomous mode (no approval prompts)
                // Note: --full-auto and --dangerously-bypass-approvals-and-sandbox are mutually exclusive
                cmd.arg("exec")
                    .arg("--full-auto");

                // Pass model from OPENAI_MODEL environment variable
                if let Ok(model) = std::env::var("OPENAI_MODEL") {
                    if !model.is_empty() {
                        cmd.arg("-m").arg(&model);
                        info!(model = %model, "Codex: using model from OPENAI_MODEL");
                    }
                }
            }
        }

        cmd
    }

    /// Inject environment variables for the CLI backend.
    fn inject_cli_env(&self, cmd: &mut Command, backend: CliBackend) {
        match backend {
            CliBackend::Claude => {
                // Claude uses ANTHROPIC_API_KEY
                if let Some(proxy_url) = &self.proxy_url {
                    cmd.env(
                        "ANTHROPIC_BASE_URL",
                        proxy_url.trim_end_matches("/v1").trim_end_matches('/'),
                    );
                    if let Some(api_key) = &self.proxy_api_key {
                        cmd.env("ANTHROPIC_API_KEY", api_key);
                    }
                } else {
                    cmd.env(
                        "ANTHROPIC_API_KEY",
                        std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
                    );
                }
            }
            CliBackend::Codex => {
                // Codex uses OPENAI_API_KEY and OPENAI_BASE_URL
                if let Some(proxy_url) = &self.proxy_url {
                    // Codex expects OpenAI-compatible endpoint
                    cmd.env(
                        "OPENAI_BASE_URL",
                        proxy_url.trim_end_matches("/v1").trim_end_matches('/'),
                    );
                    if let Some(api_key) = &self.proxy_api_key {
                        cmd.env("OPENAI_API_KEY", api_key);
                    }
                } else {
                    // Pass through OPENAI_API_KEY from environment
                    cmd.env(
                        "OPENAI_API_KEY",
                        std::env::var("OPENAI_API_KEY").unwrap_or_default(),
                    );
                    // Pass through OPENAI_BASE_URL from environment (for custom gateways like Fireworks)
                    // Also support OPENAI_API_URL for backwards compatibility
                    if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
                        cmd.env("OPENAI_BASE_URL", base_url);
                    } else if let Ok(api_url) = std::env::var("OPENAI_API_URL") {
                        // Convert chat/completions URL to base URL
                        let base_url = api_url
                            .trim_end_matches("/chat/completions")
                            .trim_end_matches("/completions")
                            .trim_end_matches('/');
                        cmd.env("OPENAI_BASE_URL", base_url);
                    }
                }
            }
        }

        // Common LLM environment variables
        Self::inject_llm_env(cmd);
    }

    fn plugin_dir(target: &Path) -> PathBuf {
        target.join(".claude").join("plugins").join("orchestration")
    }

    /// Get the Codex plugin directory (source location with .codex-plugin/plugin.json)
    fn codex_plugin_dir() -> PathBuf {
        // The orchestration plugin is in the AgentFlow repository root
        // It contains .codex-plugin/plugin.json manifest for Codex
        PathBuf::from("orchestration/plugin")
    }

    /// Spawn a FORGE process (long-running) with specified CLI backend.
    pub async fn spawn_forge_with_backend(
        &self,
        pair_id: &str,
        ticket_id: &str,
        worktree: &Path,
        shared: &Path,
        backend: CliBackend,
    ) -> Result<Child> {
        info!(
            pair = pair_id,
            ticket = ticket_id,
            worktree = %worktree.display(),
            backend = ?backend,
            "Spawning FORGE process"
        );

        // Build the initial prompt for FORGE
        let initial_prompt = self.build_forge_prompt(shared);
        let settings_path = worktree.join(".claude").join("settings.json");
        let plugin_dir = Self::plugin_dir(worktree);

        let mut cmd = self.build_cli_command(backend, worktree, shared);

        // Add backend-specific arguments
        match backend {
            CliBackend::Claude => {
                cmd.arg("--settings")
                    .arg(&settings_path)
                    .arg("--plugin-dir")
                    .arg(&plugin_dir)
                    .arg("--add-dir")
                    .arg(shared);
            }
            CliBackend::Codex => {
                // Codex uses a marketplace file at ~/.agents/plugins/marketplace.json
                // to list available plugins. The plugin directory should contain
                // a .codex-plugin/plugin.json manifest.
                // See: https://developers.openai.com/codex/plugins/build

                let home = std::env::var("HOME")
                    .or_else(|_| std::env::var("USERPROFILE"))
                    .unwrap_or_else(|_| "/tmp".to_string());

                // Create marketplace directory
                let agents_dir = PathBuf::from(&home).join(".agents").join("plugins");
                if !agents_dir.exists() {
                    std::fs::create_dir_all(&agents_dir)
                        .context("Failed to create .agents/plugins directory")?;
                }

                let marketplace_file = agents_dir.join("marketplace.json");

                // Read existing marketplace or create new one
                let mut marketplace: serde_json::Value = if marketplace_file.exists() {
                    let content = std::fs::read_to_string(&marketplace_file)
                        .context("Failed to read marketplace.json")?;
                    serde_json::from_str(&content).unwrap_or_else(|_| {
                        serde_json::json!({
                            "name": "local-plugins",
                            "plugins": []
                        })
                    })
                } else {
                    serde_json::json!({
                        "name": "local-plugins",
                        "interface": {
                            "displayName": "Local Plugins"
                        },
                        "plugins": []
                    })
                };

                // Add orchestration plugin entry if not already present
                // Use the source plugin directory that contains .codex-plugin/plugin.json
                let codex_plugin_source = Self::codex_plugin_dir();
                let plugin_entry = serde_json::json!({
                    "name": "orchestration",
                    "source": {
                        "source": "local",
                        "path": codex_plugin_source.to_string_lossy().to_string()
                    },
                    "policy": {
                        "installation": "AVAILABLE",
                        "authentication": "ON_INSTALL"
                    },
                    "category": "Productivity"
                });

                if let Some(plugins) = marketplace
                    .get_mut("plugins")
                    .and_then(|p| p.as_array_mut())
                {
                    if !plugins.iter().any(|p| p["name"] == "orchestration") {
                        plugins.push(plugin_entry);
                    }
                }

                // Write updated marketplace.json
                std::fs::write(
                    &marketplace_file,
                    serde_json::to_string_pretty(&marketplace)?,
                )
                .context("Failed to write marketplace.json")?;
            }
        }

        cmd.env("SPRINTLESS_PAIR_ID", pair_id)
            .env("SPRINTLESS_TICKET_ID", ticket_id)
            .env("SPRINTLESS_SEGMENT", "")
            .env(
                "SPRINTLESS_WORKTREE",
                worktree.to_string_lossy().to_string(),
            )
            .env("SPRINTLESS_SHARED", shared.to_string_lossy().to_string())
            .env("SPRINTLESS_GITHUB_TOKEN", &self.github_token);

        self.inject_cli_env(&mut cmd, backend);

        cmd.current_dir(worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Set Redis URL if provided, otherwise use filesystem-based state
        if let Some(redis_url) = &self.redis_url {
            cmd.env("SPRINTLESS_REDIS_URL", redis_url);
        } else {
            cmd.env(
                "SPRINTLESS_STATE_FILE",
                shared.join("state.json").to_string_lossy().to_string(),
            );
        }

        let mut child = cmd.spawn().context("Failed to spawn FORGE process")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(initial_prompt.as_bytes())
                .await
                .context("Failed to write FORGE prompt to stdin")?;
            stdin
                .shutdown()
                .await
                .context("Failed to close FORGE stdin")?;
        }

        // Capture and log stdout/stderr in background
        let log_dir = shared.join("logs");
        tokio::fs::create_dir_all(&log_dir).await?;

        if let Some(stdout) = child.stdout.take() {
            let stdout_log = log_dir.join("forge-stdout.log");
            let pair_id_clone = pair_id.to_string();
            tokio::spawn(async move {
                Self::stream_to_file(stdout, stdout_log, &pair_id_clone, "FORGE-OUT").await;
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let stderr_log = log_dir.join("forge-stderr.log");
            let pair_id_clone = pair_id.to_string();
            tokio::spawn(async move {
                Self::stream_to_file(stderr, stderr_log, &pair_id_clone, "FORGE-ERR").await;
            });
        }

        info!(pair = pair_id, pid = ?child.id(), "FORGE process spawned");
        Ok(child)
    }

    /// Spawn a FORGE process (long-running) using default backend.
    pub async fn spawn_forge(
        &self,
        pair_id: &str,
        ticket_id: &str,
        worktree: &Path,
        shared: &Path,
    ) -> Result<Child> {
        self.spawn_forge_with_backend(pair_id, ticket_id, worktree, shared, self.default_backend)
            .await
    }

    pub async fn spawn_forge_resume(
        &self,
        pair_id: &str,
        ticket_id: &str,
        worktree: &Path,
        shared: &Path,
    ) -> Result<Child> {
        info!(
            pair = pair_id,
            ticket = ticket_id,
            "Spawning FORGE process (resume mode)"
        );

        self.spawn_forge(pair_id, ticket_id, worktree, shared).await
    }

    pub async fn spawn_forge_for_pr(
        &self,
        pair_id: &str,
        ticket_id: &str,
        worktree: &Path,
        shared: &Path,
    ) -> Result<Child> {
        info!(
            pair = pair_id,
            ticket = ticket_id,
            "Spawning FORGE process (PR creation mode)"
        );

        let initial_prompt = self.build_forge_pr_prompt(shared);
        let settings_path = worktree.join(".claude").join("settings.json");
        let plugin_dir = Self::plugin_dir(worktree);

        let mut cmd = Command::new(&self.claude_path);
        cmd.arg("--print")
            .arg("--dangerously-skip-permissions")
            .arg("--settings")
            .arg(&settings_path)
            .arg("--plugin-dir")
            .arg(&plugin_dir)
            .arg("--add-dir")
            .arg(shared)
            .env("SPRINTLESS_PAIR_ID", pair_id)
            .env("SPRINTLESS_TICKET_ID", ticket_id)
            .env("SPRINTLESS_SEGMENT", "")
            .env(
                "SPRINTLESS_WORKTREE",
                worktree.to_string_lossy().to_string(),
            )
            .env("SPRINTLESS_SHARED", shared.to_string_lossy().to_string())
            .env("SPRINTLESS_GITHUB_TOKEN", &self.github_token);

        if let Some(proxy_url) = &self.proxy_url {
            Self::inject_proxy_env(
                &mut cmd,
                "forge-key",
                proxy_url,
                self.proxy_api_key.as_deref(),
            );
        } else {
            cmd.env(
                "ANTHROPIC_API_KEY",
                std::env::var("ANTHROPIC_API_KEY").unwrap_or_default(),
            );
            Self::inject_llm_env(&mut cmd);
        }

        cmd.current_dir(worktree)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        if let Some(redis_url) = &self.redis_url {
            cmd.env("SPRINTLESS_REDIS_URL", redis_url);
        } else {
            cmd.env(
                "SPRINTLESS_STATE_FILE",
                shared.join("state.json").to_string_lossy().to_string(),
            );
        }

        let mut child = cmd
            .spawn()
            .context("Failed to spawn FORGE process (PR mode)")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(initial_prompt.as_bytes())
                .await
                .context("Failed to write FORGE PR prompt to stdin")?;
            stdin
                .shutdown()
                .await
                .context("Failed to close FORGE stdin")?;
        }

        let log_dir = shared.join("logs");
        tokio::fs::create_dir_all(&log_dir).await?;

        if let Some(stdout) = child.stdout.take() {
            let stdout_log = log_dir.join("forge-stdout.log");
            let pair_id_clone = pair_id.to_string();
            tokio::spawn(async move {
                Self::stream_to_file(stdout, stdout_log, &pair_id_clone, "FORGE-OUT").await;
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let stderr_log = log_dir.join("forge-stderr.log");
            let pair_id_clone = pair_id.to_string();
            tokio::spawn(async move {
                Self::stream_to_file(stderr, stderr_log, &pair_id_clone, "FORGE-ERR").await;
            });
        }

        info!(pair = pair_id, pid = ?child.id(), "FORGE process (PR mode) spawned");
        Ok(child)
    }

    /// Spawn a SENTINEL process (ephemeral, for single evaluation).
    /// Backward-compatible overload using default timeout.
    pub async fn spawn_sentinel(
        &self,
        pair_id: &str,
        ticket_id: &str,
        mode: SentinelMode,
        worktree: &Path,
        shared: &Path,
    ) -> Result<Child> {
        self.spawn_sentinel_with_timeout(pair_id, ticket_id, mode, worktree, shared, 300)
            .await
    }

    /// Spawn a SENTINEL process with an explicit timeout using the default backend.
    pub async fn spawn_sentinel_with_timeout(
        &self,
        pair_id: &str,
        ticket_id: &str,
        mode: SentinelMode,
        worktree: &Path,
        shared: &Path,
        timeout_secs: u64,
    ) -> Result<Child> {
        self.spawn_sentinel_with_backend(pair_id, ticket_id, mode, worktree, shared, timeout_secs, self.default_backend)
            .await
    }

    /// Spawn a SENTINEL process with an explicit backend.
    pub async fn spawn_sentinel_with_backend(
        &self,
        pair_id: &str,
        ticket_id: &str,
        mode: SentinelMode,
        worktree: &Path,
        shared: &Path,
        timeout_secs: u64,
        backend: CliBackend,
    ) -> Result<Child> {
        let segment = mode.segment_value();

        info!(
            pair = pair_id,
            ticket = ticket_id,
            mode = ?mode,
            segment = %segment,
            backend = ?backend,
            "Spawning SENTINEL process (ephemeral)"
        );

        // Build the initial prompt for SENTINEL based on mode
        let initial_prompt = self.build_sentinel_prompt(shared, &mode);

        let mut cmd = self.build_cli_command(backend, worktree, shared);

        // Add backend-specific arguments
        match backend {
            CliBackend::Claude => {
                let settings_path = shared.join(".claude").join("settings.json");
                let plugin_dir = Self::plugin_dir(shared);
                cmd.arg("--output-format")
                    .arg("json")
                    .arg("--settings")
                    .arg(&settings_path)
                    .arg("--plugin-dir")
                    .arg(&plugin_dir)
                    .arg("--add-dir")
                    .arg(worktree)
                    .arg("--no-session-persistence");
            }
            CliBackend::Codex => {
                // Codex exec mode - additional flags for non-interactive execution
                // The --dangerously-bypass-approvals-and-sandbox is already added in build_cli_command
                cmd.arg("--json")
                    .arg("--ephemeral")
                    .arg("-C")
                    .arg(shared);
            }
        }

        cmd.env("SPRINTLESS_PAIR_ID", pair_id)
            .env("SPRINTLESS_TICKET_ID", ticket_id)
            .env("SPRINTLESS_SEGMENT", &segment)
            .env(
                "SPRINTLESS_WORKTREE",
                worktree.to_string_lossy().to_string(),
            )
            .env("SPRINTLESS_SHARED", shared.to_string_lossy().to_string())
            .env("SPRINTLESS_GITHUB_TOKEN", &self.github_token)
            .env("SPRINTLESS_SENTINEL_TIMEOUT_SECS", timeout_secs.to_string());

        self.inject_cli_env(&mut cmd, backend);

        cmd.current_dir(shared)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // Set Redis URL if provided, otherwise use filesystem-based state
        if let Some(redis_url) = &self.redis_url {
            cmd.env("SPRINTLESS_REDIS_URL", redis_url);
        } else {
            cmd.env(
                "SPRINTLESS_STATE_FILE",
                shared.join("state.json").to_string_lossy().to_string(),
            );
        }

        let mut child = cmd.spawn().context("Failed to spawn SENTINEL process")?;

        if let Some(mut stdin) = child.stdin.take() {
            stdin
                .write_all(initial_prompt.as_bytes())
                .await
                .context("Failed to write SENTINEL prompt to stdin")?;
            stdin
                .shutdown()
                .await
                .context("Failed to close SENTINEL stdin")?;
        }

        // Capture and log stdout/stderr in background
        let log_dir = shared.join("logs");
        tokio::fs::create_dir_all(&log_dir).await?;

        let mode_str = format!("{:?}", mode);
        if let Some(stdout) = child.stdout.take() {
            let stdout_log = log_dir.join(format!("sentinel-{}-stdout.log", mode_str));
            let pair_id_clone = pair_id.to_string();
            tokio::spawn(async move {
                Self::stream_to_file(stdout, stdout_log, &pair_id_clone, "SENTINEL-OUT").await;
            });
        }

        if let Some(stderr) = child.stderr.take() {
            let stderr_log = log_dir.join(format!("sentinel-{}-stderr.log", mode_str));
            let pair_id_clone = pair_id.to_string();
            tokio::spawn(async move {
                Self::stream_to_file(stderr, stderr_log, &pair_id_clone, "SENTINEL-ERR").await;
            });
        }

        info!(pair = pair_id, pid = ?child.id(), mode = ?mode, "SENTINEL process spawned");
        Ok(child)
    }

    /// Wait for a process to complete with timeout.
    pub async fn wait_with_timeout(
        &self,
        child: &mut Child,
        timeout: Duration,
    ) -> Result<ProcessOutcome> {
        match tokio::time::timeout(timeout, child.wait()).await {
            Ok(Ok(status)) => {
                if status.success() {
                    Ok(ProcessOutcome::Success)
                } else {
                    warn!(exit_code = ?status.code(), "Process exited with error");
                    Ok(ProcessOutcome::Failed {
                        exit_code: status.code(),
                    })
                }
            }
            Ok(Err(e)) => {
                error!(error = %e, "Failed to wait for process");
                Err(anyhow!("Failed to wait for process: {}", e))
            }
            Err(_) => {
                warn!("Process timed out, killing");
                child
                    .kill()
                    .await
                    .context("Failed to kill timed-out process")?;
                Ok(ProcessOutcome::Timeout)
            }
        }
    }

    /// Kill a process.
    pub async fn kill(&self, child: &mut Child) -> Result<()> {
        info!(pid = ?child.id(), "Killing process");
        child.kill().await.context("Failed to kill process")?;
        Ok(())
    }

    /// Check if a process is still running.
    pub async fn is_running(&self, child: &mut Child) -> bool {
        // Try to get exit status without blocking
        matches!(child.try_wait(), Ok(None))
    }

    /// Build the initial prompt for FORGE based on current state.
    fn build_forge_prompt(&self, shared: &Path) -> String {
        let handoff_path = shared.join("HANDOFF.md");
        let ticket_path = shared.join("TICKET.md");
        let task_path = shared.join("TASK.md");
        let contract_path = shared.join("CONTRACT.md");
        let plan_path = shared.join("PLAN.md");
        let ci_fix_path = shared.join("CI_FIX.md");
        let conflict_path = shared.join("CONFLICT_RESOLUTION.md");
        let shared_path = shared.display();

        // CI fix / conflict rework takes priority over CONTRACT.md AGREED —
        // otherwise FORGE would re-enter implementation mode and ignore the
        // fix instructions in TASK.md.
        if ci_fix_path.exists() || conflict_path.exists() {
            return self.rework_prompt(shared);
        }

        if handoff_path.exists() {
            // Resume mode - read handoff and continue
            let handoff = std::fs::read_to_string(&handoff_path)
                .unwrap_or_else(|_| "Could not read HANDOFF.md".to_string());

            format!(
                "You are FORGE, an autonomous coding agent. You are resuming work after a context reset.\n\n\
                IMPORTANT - Directory Structure:\n\
                - CURRENT DIRECTORY (worktree): Write ALL source code, tests, package.json here\n\
                - SHARED DIRECTORY ({}): Read/write PLAN.md, WORKLOG.md, STATUS.json here\n\n\
                VALID STATUS.json VALUES — you MUST use one of these exact strings in the \"status\" field:\n\
                - \"PR_OPENED\" — work complete, PR created (include pr_url, pr_number, branch)\n\
                - \"COMPLETE\" — all work done, PR creation deferred to harness\n\
                - \"BLOCKED\" — cannot proceed (include reason, blockers)\n\
                - \"FUEL_EXHAUSTED\" — budget/tokens exhausted\n\
                - \"PENDING_REVIEW\" — work paused, waiting for review\n\
                - \"AWAITING_SENTINEL_REVIEW\" — segment done, waiting for SENTINEL\n\
                - \"APPROVED_READY\" — changes requested by SENTINEL addressed\n\
                - \"SEGMENT_N_DONE\" — segment N complete (e.g. SEGMENT_1_DONE)\n\
                Do NOT use any other status value — it will be treated as BLOCKED and your work wasted.\n\n\
                CRITICAL: After each commit, you MUST push to remote:\n\
                - git push -u origin HEAD (first push) or git push (subsequent)\n\
                Without pushing, your work will NOT be visible on GitHub.\n\n\
                Read the handoff document and continue from the exact next step:\n\n\
                --- HANDOFF.md ---\n{}\n\n\
                Continue exactly where the previous session left off. Do not repeat work already done.",
                shared_path, handoff
            )
        } else if contract_path.exists() {
            // Check contract status for plan revision
            let contract = std::fs::read_to_string(&contract_path)
                .unwrap_or_else(|_| "Could not read CONTRACT.md".to_string());

            if contract.contains("status: ISSUES") || contract.contains("status: \"ISSUES\"") {
                // Plan was rejected - need to revise
                let plan = std::fs::read_to_string(&plan_path)
                    .unwrap_or_else(|_| "No PLAN.md found".to_string());
                let ticket = std::fs::read_to_string(&ticket_path)
                    .unwrap_or_else(|_| "No TICKET.md found".to_string());

                format!(
                    "You are FORGE. Your plan was REJECTED. Rewrite {}/PLAN.md now.\n\n\
                    --- TICKET.md ---\n{}\n\n\
                    --- Current PLAN.md ---\n{}\n\n\
                    --- REJECTION ---\n{}\n\n\
                    IMPORTANT - Directory Structure:\n\
                    - CURRENT DIRECTORY (worktree): Source code goes here\n\
                    - SHARED DIRECTORY ({}): PLAN.md, WORKLOG.md, STATUS.json go here\n\n\
                    VALID STATUS.json VALUES — use only these exact strings:\n\
                    \"PR_OPENED\", \"COMPLETE\", \"BLOCKED\", \"FUEL_EXHAUSTED\", \"PENDING_REVIEW\",\n\
                    \"AWAITING_SENTINEL_REVIEW\", \"APPROVED_READY\", \"SEGMENT_N_DONE\"\n\
                    Do NOT invent status values — they will be treated as BLOCKED.\n\n\
                    Use GitHub MCP to fetch the issue. Read codebase in current directory. \
                    Write {}/PLAN.md with:\n\
                    - ## Understanding: What we're building\n\
                    - ## Segments: Specific files in CURRENT DIRECTORY like 'src/counter.ts'\n\
                    - ## Files Changed: Every file you'll touch (all in current directory)\n\
                    - ## Risks: What could go wrong",
                    shared_path, ticket, plan, contract, shared_path, shared_path
                )
            } else if contract.contains("status: AGREED") || contract.contains("status: \"AGREED\"")
            {
                // Contract agreed - continue implementation
                let worklog_path = shared.join("WORKLOG.md");
                let worklog = if worklog_path.exists() {
                    std::fs::read_to_string(&worklog_path)
                        .unwrap_or_else(|_| "No WORKLOG.md found".to_string())
                } else {
                    "No WORKLOG.md yet - start implementation".to_string()
                };

                format!(
                    "You are FORGE, an autonomous coding agent. Your plan was approved.\n\n\
                    --- CONTRACT.md ---\n{}\n\n\
                    --- WORKLOG.md ---\n{}\n\n\
                    IMPORTANT - Directory Structure:\n\
                    - CURRENT DIRECTORY (worktree): Write ALL source code, tests, package.json here\n\
                    - SHARED DIRECTORY ({}): Write WORKLOG.md, STATUS.json here\n\n\
                    VALID STATUS.json VALUES — use only these exact strings in the \"status\" field:\n\
                    - \"PR_OPENED\" — work complete, PR created (include pr_url, pr_number, branch)\n\
                    - \"COMPLETE\" — all work done, PR creation deferred to harness\n\
                    - \"BLOCKED\" — cannot proceed (include reason, blockers)\n\
                    - \"FUEL_EXHAUSTED\" — budget/tokens exhausted\n\
                    - \"PENDING_REVIEW\" — work paused, waiting for review\n\
                    - \"AWAITING_SENTINEL_REVIEW\" — segment done, waiting for SENTINEL\n\
                    - \"APPROVED_READY\" — changes requested by SENTINEL addressed\n\
                    - \"SEGMENT_N_DONE\" — segment N complete (e.g. SEGMENT_1_DONE)\n\
                    Do NOT use any other status value — it will be treated as BLOCKED and your work wasted.\n\n\
                    IMPLEMENTATION WORKFLOW (one segment at a time):\n\
                    1. Implement ONE segment from PLAN.md\n\
                    2. Write tests for that segment\n\
                    3. Update {}/WORKLOG.md with segment progress\n\
                    4. Commit and push your changes:\n\
                       - git add -A && git commit -m \"Segment N: <description>\"\n\
                       - git push -u origin HEAD (first push) or git push (subsequent)\n\
                    5. WAIT for SENTINEL review - SENTINEL will evaluate your segment\n\
                    6. If APPROVED, continue to next segment\n\
                    7. If CHANGES_REQUESTED, fix issues and update WORKLOG.md\n\
                    8. Repeat until all segments complete\n\
                    9. When ALL segments APPROVED, SENTINEL does final review\n\
                    10. After final APPROVAL, create PR\n\n\
                    CRITICAL: You MUST push to remote after each commit or your work will NOT be visible on GitHub.\n\
                    You have full permissions. Install deps with 'npm install'. \
                    Document progress in {}/WORKLOG.md.",
                    contract, worklog, shared_path, shared_path, shared_path
                )
            } else {
                // Unknown contract state - treat as new session
                self.new_session_prompt(&ticket_path, &task_path, shared)
            }
        } else if plan_path.exists() {
            // PLAN.md exists but no CONTRACT.md yet - SENTINEL has not reviewed the plan.
            // Since --print mode exits after one response, we should NOT respawn FORGE
            // to wait for SENTINEL. Instead, just exit cleanly. The harness event loop
            // will spawn SENTINEL and then respawn FORGE once CONTRACT.md is written.
            info!("PLAN.md exists but no CONTRACT.md - FORGE has nothing to do until SENTINEL reviews");

            // Write a minimal WORKLOG.md so the harness knows progress was made
            let worklog_path = shared.join("WORKLOG.md");
            if !worklog_path.exists() {
                let plan = std::fs::read_to_string(&plan_path)
                    .unwrap_or_else(|_| "No PLAN.md found".to_string());
                format!(
                    "You are FORGE. Your PLAN.md has been submitted for review.\n\n\
                    --- PLAN.md ---\n{}\n\n\
                    IMPORTANT: Do NOT write any code or modify any files. Your plan is pending SENTINEL review.\n\
                    Simply respond with: 'PLAN.md submitted for SENTINEL review. Awaiting CONTRACT.md.'\n\
                    Do NOT rewrite PLAN.md. Do NOT start implementation. Wait for CONTRACT.md.",
                    plan
                )
            } else {
                // WORKLOG exists but no CONTRACT - implementation was started before contract?
                // Fall through to new session
                self.new_session_prompt(&ticket_path, &task_path, shared)
            }
        } else {
            // New session - read ticket and task
            self.new_session_prompt(&ticket_path, &task_path, shared)
        }
    }

    /// Build the prompt for a new session.
    fn new_session_prompt(&self, ticket_path: &Path, task_path: &Path, shared: &Path) -> String {
        let ticket = std::fs::read_to_string(ticket_path)
            .unwrap_or_else(|_| "No TICKET.md found".to_string());
        let task =
            std::fs::read_to_string(task_path).unwrap_or_else(|_| "No TASK.md found".to_string());
        let shared_path = shared.display();

        format!(
            "You are FORGE. Write a detailed implementation plan to {}/PLAN.md.\n\n\
            --- TICKET.md ---\n{}\n\n\
            --- TASK.md ---\n{}\n\n\
            IMPORTANT - Directory Structure:\n\
            - CURRENT DIRECTORY (worktree): Write ALL source code, tests, package.json here\n\
            - SHARED DIRECTORY ({}): Write PLAN.md, WORKLOG.md, STATUS.json here\n\n\
            VALID STATUS.json VALUES — use only these exact strings in the \"status\" field:\n\
            \"PR_OPENED\", \"COMPLETE\", \"BLOCKED\", \"FUEL_EXHAUSTED\", \"PENDING_REVIEW\",\n\
            \"AWAITING_SENTINEL_REVIEW\", \"APPROVED_READY\", \"SEGMENT_N_DONE\"\n\
            Do NOT invent status values — any other value will be treated as BLOCKED.\n\n\
            STEPS (do these NOW):\n\
            1. Read {}/TICKET.md and {}/TASK.md from the shared directory\n\
            2. Read the codebase in current directory: README.md, package.json/Cargo.toml, src/\n\
            3. Write PLAN.md to shared directory with:\n\
               - ## Understanding: What you're building\n\
               - ## Segments: 1-3 files each, specific file paths in CURRENT DIRECTORY\n\
               - Do NOT create a verification-only segment whose only work is running lint/typecheck/tests\n\
               - ## Files Changed: List every file you'll touch (all in current directory)\n\
               - ## Risks: What could go wrong\n\n\
             Write PLAN.md to shared directory now. Do NOT write any code yet - only the plan.",
            shared_path, ticket, task, shared_path, shared_path, shared_path
        )
    }

    /// Build the prompt for CI fix or conflict rework.
    ///
    /// This is used when CI_FIX.md or CONFLICT_RESOLUTION.md exists in the
    /// shared directory. It must take priority over CONTRACT.md AGREED so that
    /// FORGE actually addresses the CI failure / merge conflict instead of
    /// re-entering the normal segment implementation workflow.
    fn rework_prompt(&self, shared: &Path) -> String {
        let task_path = shared.join("TASK.md");
        let worklog_path = shared.join("WORKLOG.md");
        let ci_fix_path = shared.join("CI_FIX.md");
        let conflict_path = shared.join("CONFLICT_RESOLUTION.md");
        let shared_path = shared.display();

        let mode = if ci_fix_path.exists() {
            "CI FIX"
        } else {
            "CONFLICT RESOLUTION"
        };

        let rework_content = if ci_fix_path.exists() {
            std::fs::read_to_string(&ci_fix_path)
                .unwrap_or_else(|_| "Could not read CI_FIX.md".to_string())
        } else {
            std::fs::read_to_string(&conflict_path)
                .unwrap_or_else(|_| "Could not read CONFLICT_RESOLUTION.md".to_string())
        };

        let task =
            std::fs::read_to_string(&task_path).unwrap_or_else(|_| "No TASK.md found".to_string());
        let worklog = if worklog_path.exists() {
            std::fs::read_to_string(&worklog_path)
                .unwrap_or_else(|_| "No WORKLOG.md found".to_string())
        } else {
            "No WORKLOG.md yet".to_string()
        };

        format!(
            "You are FORGE, an autonomous coding agent. This is a {mode} cycle — NOT normal implementation.\n\n\
            --- TASK.md ---\n{task}\n\n\
            --- {mode} DETAILS ---\n{rework_content}\n\n\
            --- WORKLOG.md (previous progress) ---\n{worklog}\n\n\
            IMPORTANT - Directory Structure:\n\
            - CURRENT DIRECTORY (worktree): Write ALL source code, tests, package.json here\n\
            - SHARED DIRECTORY ({shared_path}): Write WORKLOG.md, STATUS.json here\n\n\
            VALID STATUS.json VALUES — use only these exact strings in the \"status\" field:\n\
            - \"PR_OPENED\" — work complete, PR created (include pr_url, pr_number, branch)\n\
            - \"COMPLETE\" — all work done, PR creation deferred to harness\n\
            - \"BLOCKED\" — cannot proceed (include reason, blockers)\n\
            - \"FUEL_EXHAUSTED\" — budget/tokens exhausted\n\
            - \"PENDING_REVIEW\" — work paused, waiting for review\n\
            Do NOT use any other status value — it will be treated as BLOCKED and your work wasted.\n\n\
            CRITICAL: Follow the instructions in TASK.md exactly. This is a {mode} cycle — \
            do NOT re-implement already-completed segments. Focus ONLY on fixing the issues \
            described in the {mode} details above.\n\n\
            You MUST update {shared_path}/WORKLOG.md as you work — the watchdog will kill your \
            process if WORKLOG.md is not updated within 20 minutes.\n\n\
            After fixing issues, commit and push:\n\
            - git add -A && git commit -m \"{mode}: <description>\"\n\
            - git push (or git push -u origin HEAD if first push)\n\n\
            If a PR already exists for this branch, do NOT create a new one — just push and update STATUS.json.",
            mode = mode,
            task = task,
            rework_content = rework_content,
            worklog = worklog,
            shared_path = shared_path,
        )
    }

    /// Build the prompt for PR creation after final SENTINEL approval.
    fn build_forge_pr_prompt(&self, shared: &Path) -> String {
        let shared_path = shared.display();
        let final_review_path = shared.join("final-review.md");
        let final_review = std::fs::read_to_string(&final_review_path)
            .unwrap_or_else(|_| "No final-review.md found".to_string());
        let contract_path = shared.join("CONTRACT.md");
        let contract = std::fs::read_to_string(&contract_path)
            .unwrap_or_else(|_| "No CONTRACT.md found".to_string());
        let worklog_path = shared.join("WORKLOG.md");
        let worklog = std::fs::read_to_string(&worklog_path)
            .unwrap_or_else(|_| "No WORKLOG.md found".to_string());

        format!(
            "You are FORGE. SENTINEL has APPROVED and CERTIFIED your implementation. Create the PR.\n\n\
            --- FINAL REVIEW (SENTINEL CERTIFIED) ---\n{}\n\n\
            --- CONTRACT.md ---\n{}\n\n\
            --- WORKLOG.md ---\n{}\n\n\
            IMPORTANT: SENTINEL has reviewed and certified this code.\n\
            The final-review.md contains SENTINEL's signature and certification.\n\n\
            DIRECTORY STRUCTURE:\n\
            - CURRENT DIRECTORY (worktree): Source code is here\n\
            - SHARED DIRECTORY ({}): Write STATUS.json here\n\n\
            VALID STATUS.json VALUES — use only these exact strings:\n\
            \"PR_OPENED\", \"COMPLETE\", \"BLOCKED\", \"FUEL_EXHAUSTED\", \"PENDING_REVIEW\",\n\
            \"AWAITING_SENTINEL_REVIEW\", \"APPROVED_READY\", \"SEGMENT_N_DONE\"\n\
            Do NOT use any other status value — it will be treated as BLOCKED.\n\n\
            PR CREATION STEPS:\n\
            1. Ensure all changes committed: 'git status' then commit if needed\n\
            2. Push branch: 'git push -u origin HEAD'\n\
               If push is rejected (non-fast-forward), use 'git push --force-with-lease -u origin HEAD'\n\
            3. Create PR using GitHub MCP create_pull_request:\n\
               - title: from CONTRACT summary\n\
               - body: include SENTINEL's PR description and CERTIFICATION\n\
               - head: current branch\n\
               - base: 'main'\n\
               If a PR already exists for this branch, do NOT create a new one — just update STATUS.json with the existing PR info.\n\
            4. Write {}/STATUS.json:\n\
               {{\n\
                 \"status\": \"PR_OPENED\",\n\
                 \"pr_url\": \"<pr url>\",\n\
                 \"pr_number\": <number>,\n\
                 \"branch\": \"<branch>\",\n\
                 \"sentinel_certified\": true,\n\
                 \"certification\": \"Reviewed and approved by SENTINEL\"\n\
               }}\n\n\
            Include SENTINEL's certification in PR body. This proves code quality.",
            final_review, contract, worklog, shared_path, shared_path
        )
    }

    /// Build the initial prompt for SENTINEL based on mode.
    fn build_sentinel_prompt(&self, shared: &Path, mode: &SentinelMode) -> String {
        let shared_path = shared.display();

        match mode {
            SentinelMode::PlanReview => {
                let plan_path = shared.join("PLAN.md");
                let plan = std::fs::read_to_string(&plan_path)
                    .unwrap_or_else(|_| "No PLAN.md found".to_string());
                let ticket_path = shared.join("TICKET.md");
                let ticket = std::fs::read_to_string(&ticket_path)
                    .unwrap_or_else(|_| "No TICKET.md found".to_string());

                format!(
                    "You are SENTINEL. Review this plan. Write ONLY to {}/CONTRACT.md.\n\n\
                     --- TICKET.md ---\n{}\n\n\
                     --- PLAN.md ---\n{}\n\n\
                     Check the plan has these sections:\n\
                     - ## Understanding (explains what we're building)\n\
                     - ## Segments (each with Files and Definition of Done)\n\
                     - ## Files Changed (specific file paths)\n\
                     - ## Risks (identified risks)\n\n\
                     APPROVE if all sections exist and are specific (real file paths, real criteria).\n\
                     REJECT any segment that is only verification commands and has no file list.\n\
                     REJECT if generic/placeholder content (e.g. '[Task 1 description]').\n\n\
                     ESTIMATE TIMEOUTS based on these complexity factors:\n\
                     - Number of segments (more segments = more eval time)\n\
                     - Test coverage depth (integration/e2e tests need more time than unit tests)\n\
                     - Build system requirements (compiled languages, container builds add time)\n\
                     - Number of files changed (larger diffs need more review time)\n\
                     - Cross-cutting changes (refactors, API changes affect many files)\n\n\
                     Timeout guidelines (these are BASE values, the harness adds environmental overhead):\n\
                     - Low complexity: plan_review=90s, segment_eval=180s, final_review=300s\n\
                       (1-2 segments, unit tests only, few files, simple feature)\n\
                     - Medium complexity: plan_review=120s, segment_eval=300s, final_review=480s\n\
                       (3-4 segments, integration tests, moderate files, typical feature)\n\
                     - High complexity: plan_review=180s, segment_eval=480s, final_review=720s\n\
                       (5+ segments, e2e/container builds, many files, cross-cutting refactor)\n\n\
                     Write CONTRACT.md now:\n\
                     ---\n\
                     status: AGREED | ISSUES\n\
                     summary: <one line>\n\
                     definition_of_done:\n\
                     - <criterion from plan>\n\
                     objections:\n\
                     - <specific issue or 'None'>\n\
                     timeout_profile:\n\
                       plan_review_secs: <number>\n\
                       segment_eval_secs: <number>\n\
                       final_review_secs: <number>\n\
                       complexity: low | medium | high",
                    shared_path, ticket, plan
                )
            }
            SentinelMode::SegmentEval(n) => {
                let worklog_path = shared.join("WORKLOG.md");
                let worklog = std::fs::read_to_string(&worklog_path)
                    .unwrap_or_else(|_| "No WORKLOG.md found".to_string());
                let contract_path = shared.join("CONTRACT.md");
                let contract = std::fs::read_to_string(&contract_path)
                    .unwrap_or_else(|_| "No CONTRACT.md found".to_string());

                format!(
                    "You are SENTINEL. Evaluate segment {}.\n\n\
                    --- CONTRACT.md ---\n{}\n\n\
                    --- WORKLOG.md ---\n{}\n\n\
                    SHARED: {}\n\n\
                    EVALUATE:\n\
                    1. Run tests: 'npm test' or 'cargo test'\n\
                    2. Check CONTRACT criteria all met\n\
                    3. Check test coverage - new code has tests\n\
                    4. Check standards - follows CODING.md\n\
                    5. Check for regressions - existing tests pass\n\n\
                    Write {}/segment-{}-eval.md:\n\
                    - ## Verdict: APPROVED | CHANGES_REQUESTED\n\
                    - ## Specific feedback: issues with file:line format\n\
                    - APPROVED = certified for this segment\n\
                    - CHANGES_REQUESTED = FORGE fixes and re-submits",
                    n, contract, worklog, shared_path, shared_path, n
                )
            }
            SentinelMode::FinalReview => {
                let worklog_path = shared.join("WORKLOG.md");
                let worklog = std::fs::read_to_string(&worklog_path)
                    .unwrap_or_else(|_| "No WORKLOG.md found".to_string());
                let contract_path = shared.join("CONTRACT.md");
                let contract = std::fs::read_to_string(&contract_path)
                    .unwrap_or_else(|_| "No CONTRACT.md found".to_string());

                format!(
                    "You are SENTINEL. FINAL REVIEW.\n\n\
                    --- CONTRACT.md ---\n{}\n\n\
                    --- WORKLOG.md ---\n{}\n\n\
                    SHARED: {}\n\n\
                    FINAL CHECKLIST:\n\
                    1. All segment-eval.md files show APPROVED\n\
                    2. All CONTRACT criteria verified\n\
                    3. All tests passing\n\
                    4. No regressions\n\n\
                    Write {}/final-review.md:\n\
                    - ## Verdict: APPROVED | REJECTED\n\
                    - ## Summary: what was implemented\n\
                    - ## PR description: for PR body (if APPROVED)\n\
                    - ## Certification: 'Code certified by SENTINEL - meets all acceptance criteria'\n\
                    - ## Signature: 'Reviewed and approved by SENTINEL on [date]'\n\n\
                    If APPROVED, FORGE creates PR with your description.\n\
                    If REJECTED, list issues FORGE must fix.",
                    contract, worklog, shared_path, shared_path
                )
            }
        }
    }

    /// Stream process output to a log file.
    async fn stream_to_file<T: tokio::io::AsyncRead + Unpin>(
        stream: T,
        log_path: PathBuf,
        pair_id: &str,
        prefix: &str,
    ) {
        let mut reader = BufReader::new(stream).lines();
        let mut log_file = match tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&log_path)
            .await
        {
            Ok(f) => f,
            Err(e) => {
                error!(pair = pair_id, error = %e, "Failed to open log file");
                return;
            }
        };

        while let Ok(Some(line)) = reader.next_line().await {
            debug!(pair = pair_id, prefix = prefix, "{}", line);
            if let Err(e) = log_file.write_all(format!("{}\n", line).as_bytes()).await {
                error!(pair = pair_id, error = %e, "Failed to write to log file");
                break;
            }
        }
    }
}

/// Outcome of a process execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProcessOutcome {
    /// Process completed successfully
    Success,
    /// Process failed with exit code
    Failed { exit_code: Option<i32> },
    /// Process timed out and was killed
    Timeout,
}

/// Builder for creating FORGE processes with custom configuration.
pub struct ForgeProcessBuilder {
    pair_id: String,
    ticket_id: String,
    worktree: PathBuf,
    shared: PathBuf,
    github_token: String,
    redis_url: Option<String>,
    proxy_url: Option<String>,
    extra_env: Vec<(String, String)>,
}

impl ForgeProcessBuilder {
    /// Create a new builder.
    pub fn new(
        pair_id: impl Into<String>,
        ticket_id: impl Into<String>,
        worktree: PathBuf,
        shared: PathBuf,
    ) -> Self {
        Self {
            pair_id: pair_id.into(),
            ticket_id: ticket_id.into(),
            worktree,
            shared,
            github_token: String::new(),
            redis_url: None,
            proxy_url: None,
            extra_env: Vec::new(),
        }
    }

    /// Set the GitHub token.
    pub fn github_token(mut self, token: impl Into<String>) -> Self {
        self.github_token = token.into();
        self
    }

    /// Set the Redis URL (optional - uses filesystem state if not provided).
    pub fn redis_url(mut self, url: impl Into<String>) -> Self {
        self.redis_url = Some(url.into());
        self
    }

    /// Set the LiteLLM proxy URL for per-agent model routing.
    pub fn proxy_url(mut self, url: impl Into<String>) -> Self {
        self.proxy_url = Some(url.into());
        self
    }

    /// Add an extra environment variable.
    pub fn env(mut self, key: impl Into<String>, value: impl Into<String>) -> Self {
        self.extra_env.push((key.into(), value.into()));
        self
    }

    /// Build and spawn the FORGE process.
    pub async fn spawn(self) -> Result<Child> {
        let manager = match (&self.redis_url, &self.proxy_url) {
            (Some(redis_url), Some(proxy_url)) => {
                ProcessManager::with_proxy(self.github_token, Some(redis_url.clone()), proxy_url)
            }
            (Some(redis_url), None) => ProcessManager::with_redis(self.github_token, redis_url),
            (None, Some(proxy_url)) => {
                ProcessManager::with_proxy(self.github_token, None, proxy_url)
            }
            (None, None) => ProcessManager::new(self.github_token),
        };

        let child = manager
            .spawn_forge(&self.pair_id, &self.ticket_id, &self.worktree, &self.shared)
            .await?;

        // Add extra environment variables
        // Note: This doesn't work after spawn, so we need to handle this differently
        // For now, the extra_env is not used, but could be added to the Command before spawn

        Ok(child)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn test_sentinel_mode_segment_value() {
        assert_eq!(SentinelMode::PlanReview.segment_value(), "");
        assert_eq!(SentinelMode::SegmentEval(3).segment_value(), "3");
        assert_eq!(SentinelMode::FinalReview.segment_value(), "final");
    }

    #[test]
    fn test_plan_review_prompt_uses_shared_absolute_paths() {
        let manager = ProcessManager::new("ghp_test");
        let prompt =
            manager.build_sentinel_prompt(Path::new("/tmp/shared"), &SentinelMode::PlanReview);

        assert!(prompt.contains("--- TICKET.md ---"));
        assert!(prompt.contains("Write ONLY to /tmp/shared/CONTRACT.md"));
        assert!(prompt.contains("status: AGREED | ISSUES"));
        assert!(prompt.contains("REJECT any segment that is only verification commands"));
        assert!(prompt.contains("REJECT if generic/placeholder content"));
        assert!(prompt.contains("definition_of_done:"));
        assert!(prompt.contains("timeout_profile:"));
        assert!(prompt.contains("plan_review_secs:"));
        assert!(prompt.contains("segment_eval_secs:"));
        assert!(prompt.contains("final_review_secs:"));
        assert!(prompt.contains("complexity: low | medium | high"));
    }

    #[test]
    fn test_new_session_prompt_discourages_verification_only_segments() {
        let manager = ProcessManager::new("ghp_test");
        let dir = tempfile::tempdir().unwrap();
        let ticket_path = dir.path().join("TICKET.md");
        let task_path = dir.path().join("TASK.md");
        std::fs::write(&ticket_path, "# Ticket").unwrap();
        std::fs::write(&task_path, "Implement it").unwrap();

        let prompt = manager.new_session_prompt(&ticket_path, &task_path, Path::new("/tmp/shared"));

        assert!(prompt.contains("Do NOT create a verification-only segment"));
    }

    #[test]
    fn test_segment_eval_prompt_uses_shared_absolute_paths() {
        let manager = ProcessManager::new("ghp_test");
        let prompt =
            manager.build_sentinel_prompt(Path::new("/tmp/shared"), &SentinelMode::SegmentEval(3));

        assert!(prompt.contains("SHARED: /tmp/shared"));
        assert!(prompt.contains("--- CONTRACT.md ---"));
        assert!(prompt.contains("Write /tmp/shared/segment-3-eval.md"));
    }

    #[test]
    fn test_final_review_prompt_uses_shared_absolute_paths() {
        let manager = ProcessManager::new("ghp_test");
        let prompt =
            manager.build_sentinel_prompt(Path::new("/tmp/shared"), &SentinelMode::FinalReview);

        assert!(prompt.contains("SHARED: /tmp/shared"));
        assert!(prompt.contains("--- CONTRACT.md ---"));
        assert!(prompt.contains("Write /tmp/shared/final-review.md"));
    }
}
