// crates/pair-harness/src/process.rs
//! Process management for FORGE and SENTINEL agents.
//!
//! Supports multiple CLI backends via the `BackendConfig` abstraction:
//! - Claude Code CLI (default)
//! - OpenAI Codex CLI
//!
//! Adding a new backend only requires implementing a new `BackendConfig` instance.

use anyhow::{anyhow, Context, Result};
use std::collections::HashMap;
use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::time::Duration;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::process::{Child, Command};
use tracing::{debug, error, info, warn};

use crate::types::CliBackend;

#[cfg(feature = "coder")]
use crate::pair::tail_truncate;

use serde::Deserialize;

/// Cross-provider process handle for local child processes and Coder exec tasks.
pub enum ManagedProcess {
    Local(Child),
    #[cfg(feature = "coder")]
    Coder(crate::coder_process::CoderTaskHandle),
    #[cfg(feature = "coder")]
    CoderJoin(Option<tokio::task::JoinHandle<Result<i32>>>),
}

/// Minimal exit status used by the pair lifecycle without tying it to
/// std::process::ExitStatus.
pub struct ManagedExitStatus {
    code: Option<i32>,
}

impl ManagedExitStatus {
    pub fn success(&self) -> bool {
        self.code == Some(0)
    }

    pub fn code(&self) -> Option<i32> {
        self.code
    }
}

impl From<std::process::ExitStatus> for ManagedExitStatus {
    fn from(status: std::process::ExitStatus) -> Self {
        ManagedExitStatus { code: status.code() }
    }
}

impl ManagedProcess {
    /// Poll whether the process has exited without blocking.
    ///
    /// Returns `Ok(None)` while still running. Remote Coder PID tasks have no
    /// synchronous poll available, so they report as still running here;
    /// completion is detected via the file-watching event loop instead.
    pub fn try_poll_exit(&mut self) -> std::io::Result<Option<ManagedExitStatus>> {
        match self {
            ManagedProcess::Local(child) => {
                child.try_wait().map(|opt| opt.map(ManagedExitStatus::from))
            }
            #[cfg(feature = "coder")]
            ManagedProcess::CoderJoin(opt) => {
                use futures::FutureExt;
                let Some(handle) = opt.as_mut() else {
                    return Ok(Some(ManagedExitStatus { code: None }));
                };
                match handle.now_or_never() {
                    None => Ok(None),
                    Some(res) => {
                        // The JoinHandle has resolved; drop the stored handle.
                        *opt = None;
                        match res {
                            Ok(Ok(code)) => Ok(Some(ManagedExitStatus { code: Some(code) })),
                            _ => Ok(Some(ManagedExitStatus { code: None })),
                        }
                    }
                }
            }
            #[cfg(feature = "coder")]
            ManagedProcess::Coder(_) => Ok(None),
        }
    }
}

/// Shell-quote a single token for safe interpolation into a `sh -c` command.
///
/// Tokens consisting solely of "safe" characters (alphanumeric plus
/// `_-./:=`) are passed through unquoted; everything else is wrapped in
/// single quotes with embedded single quotes escaped.  This is the single
/// escaping primitive used by both the Coder remote-spawn path and the
/// provisioner's transport commands, so a correctness fix here propagates
/// everywhere.
pub(crate) fn shell_quote(value: impl AsRef<OsStr>) -> String {
    let s = value.as_ref().to_string_lossy();
    if s.is_empty() {
        return "''".to_string();
    }
    if s.chars()
        .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.' | '/' | ':' | '='))
    {
        s.into_owned()
    } else {
        format!("'{}'", s.replace('\'', "'\\''"))
    }
}

/// Derive the Redis URL that a process running *inside* a Coder workspace
/// container should use to reach the same Redis instance the host
/// orchestrator connects to.
///
/// The host resolves a host-reachable URL (e.g. `redis://127.0.0.1:6379`)
/// because it probes TCP from the host and deliberately rejects the
/// `redis://redis:6379` compose-network alias (the host can't resolve it).
/// That same loopback URL is unreachable from inside the workspace
/// container, where `127.0.0.1` is the container's own loopback.  The
/// container is attached to the compose network, so the compose service
/// alias `redis` is the correct target.
///
/// An explicit override can be supplied via the `CODER_REDIS_URL` env var;
/// otherwise loopback hosts in the host URL are rewritten to `redis`.
#[cfg(feature = "coder")]
fn container_reachable_redis_url(host_redis_url: &str) -> String {
    if let Ok(override_url) = std::env::var("CODER_REDIS_URL") {
        let trimmed = override_url.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    if let Ok(parsed) = reqwest::Url::parse(host_redis_url) {
        let host = parsed.host_str().unwrap_or("redis");
        let is_loopback = matches!(host, "localhost" | "127.0.0.1" | "::1" | "0.0.0.0");
        if is_loopback {
            let port = parsed.port().unwrap_or(6379);
            return format!("redis://redis:{}", port);
        }
    }
    host_redis_url.to_string()
}

/// Build the content of a remote env-file from a `Command`'s environment.
///
/// Each entry is emitted as `export KEY='value'` so the file can be sourced
/// with `. <file>`.  The content is written to the workspace via
/// `workspace_write_file`, which base64-encodes it on the wire — so secret
/// values never appear in a shell command string or in the tracing log.
#[cfg(feature = "coder")]
fn build_env_file_content(cmd: &Command) -> String {
    let cmd = cmd.as_std();
    let mut lines = String::new();
    for (key, value) in cmd.get_envs() {
        if let Some(value) = value {
            // The key is an env-var name (always safe chars); only the value
            // needs quoting.
            lines.push_str(&format!(
                "export {}={}\n",
                key.to_string_lossy(),
                shell_quote(value)
            ));
        }
    }
    lines
}

/// Render the program + args of a `Command` as a shell command string,
/// **without** any `env KEY=VALUE …` prefix.  Environment is delivered via a
/// sourced env-file (see [`build_env_file_content`]) so that secrets are not
/// inlined into the command string.
#[cfg(feature = "coder")]
fn command_args_to_shell(cmd: &Command) -> String {
    let cmd = cmd.as_std();
    let mut parts = vec![shell_quote(cmd.get_program())];
    parts.extend(cmd.get_args().map(shell_quote));
    parts.join(" ")
}

/// Write the command's environment to a remote env-file and return
/// `(env_file_path, shell_cmd_without_envs)`.
///
/// This is the shared half of every Coder spawn: it guarantees that secrets
/// live only in the env-file (transmitted via base64, never logged) and that
/// the returned `shell_cmd` contains only the program and its arguments.
#[cfg(feature = "coder")]
async fn prepare_coder_remote_command(
    client: &coder_client::CoderClient,
    workspace_id: &str,
    pair_id: &str,
    cmd: &Command,
) -> Result<(String, String)> {
    let env_file = format!("/tmp/agentflow-{}-env", pair_id);
    let env_content = build_env_file_content(cmd);
    client
        .workspace_write_file(workspace_id, &env_file, &env_content)
        .await
        .context("Failed to write env file into Coder workspace")?;
    let shell_cmd = command_args_to_shell(cmd);
    Ok((env_file, shell_cmd))
}

/// Configuration for a specific CLI backend (Claude, Codex, etc.).
/// Encapsulates all backend-specific behavior: binary path, spawn flags,
/// environment variables, plugin handling, and provisioning details.
///
/// Adding a new backend only requires creating a new `BackendConfig` instance —
/// no changes to `ProcessManager`, `Provisioner`, or spawn logic.
pub struct BackendConfig {
    /// Binary path (from env var or default name)
    pub binary_path: PathBuf,
    /// Base flags always passed to the binary (e.g., `--print`, `--dangerously-skip-permissions`)
    pub base_flags: Vec<String>,
    /// Flags added during FORGE long-running mode
    pub forge_flags: Vec<String>,
    /// Flags added during FORGE PR-creation mode
    pub forge_pr_flags: Vec<String>,
    /// Flags added during SENTINEL ephemeral mode
    pub sentinel_flags: Vec<String>,
    /// Environment variable name for the API key
    pub api_key_env: String,
    /// Environment variable name for the base URL (proxy)
    pub base_url_env: Option<String>,
    /// Environment variable name for the model override
    pub model_env: Option<String>,
    /// Whether to set a backend-specific home dir (e.g., CODEX_HOME)
    pub home_env_var: Option<String>,
    /// Home directory relative to worktree/shared (empty = not used)
    pub home_dir_suffix: String,
    /// Plugin directory inside the target (e.g., `.claude/plugins/orchestration`)
    pub plugin_dir_rel: PathBuf,
    /// Settings file path relative to target (e.g., `.claude/settings.json`)
    pub settings_rel: PathBuf,
    /// Whether this backend needs stdin prompt injection
    pub uses_stdin_prompt: bool,
    /// MCP config file relative to target
    pub mcp_config_rel: PathBuf,
    /// Whether to run backend-specific provisioning (e.g., Codex marketplace.json)
    pub needs_extras_provisioning: bool,
    /// Extra command args for FORGE mode (e.g., --settings, --plugin-dir, --add-dir)
    pub forge_extra_args: Vec<String>,
    /// Extra command args for SENTINEL mode
    pub sentinel_extra_args: Vec<String>,
}

impl BackendConfig {
    /// Create a Claude Code backend config.
    pub fn claude(cli_path: &str, worktree: &Path, shared: &Path) -> Self {
        let binary = if cli_path.is_empty() {
            "claude"
        } else {
            cli_path
        };
        let settings_path = worktree.join(".claude").join("settings.json");
        let plugin_dir = worktree
            .join(".claude")
            .join("plugins")
            .join("orchestration");
        let forge_extra = vec![
            "--settings".into(),
            settings_path.to_string_lossy().to_string(),
            "--plugin-dir".into(),
            plugin_dir.to_string_lossy().to_string(),
            // --add-dir shared is no longer needed: the shared directory
            // (.pair-shared/) now lives inside the worktree, so it is
            // already accessible as part of the workspace.
        ];
        let sentinel_settings = shared.join(".claude").join("settings.json");
        let sentinel_plugin_dir = shared.join(".claude").join("plugins").join("orchestration");
        let sentinel_extra = vec![
            "--settings".into(),
            sentinel_settings.to_string_lossy().to_string(),
            "--plugin-dir".into(),
            sentinel_plugin_dir.to_string_lossy().to_string(),
            // --add-dir worktree is not needed: the shared directory
            // (.pair-shared/) lives inside the worktree, so it is already
            // accessible as part of the workspace.
        ];
        Self {
            binary_path: PathBuf::from(binary),
            base_flags: vec![
                "--print".into(),
                "--dangerously-skip-permissions".into(),
                "--output-format".into(),
                "stream-json".into(),
                "--verbose".into(),
            ],
            forge_flags: vec![],
            forge_pr_flags: vec![],
            sentinel_flags: vec![
                "--output-format".into(),
                "json".into(),
                "--no-session-persistence".into(),
            ],
            api_key_env: "ANTHROPIC_API_KEY".into(),
            base_url_env: Some("ANTHROPIC_BASE_URL".into()),
            model_env: Some("ANTHROPIC_MODEL".into()),
            home_env_var: None,
            home_dir_suffix: String::new(),
            plugin_dir_rel: PathBuf::from(".claude")
                .join("plugins")
                .join("orchestration"),
            settings_rel: PathBuf::from(".claude").join("settings.json"),
            uses_stdin_prompt: true,
            mcp_config_rel: PathBuf::from(".claude").join("mcp.json"),
            needs_extras_provisioning: true,
            forge_extra_args: forge_extra,
            sentinel_extra_args: sentinel_extra,
        }
    }

    /// Create a Codex CLI backend config.
    ///
    /// Supports two provider modes, selected via the `CODEX_PROVIDER` env var
    /// (or auto-detected from the environment):
    ///
    /// - **`fireworks`** (default when `FIREWORKS_API_KEY` is set): Uses a
    ///   custom model provider with `supports_websockets=false` so codex uses
    ///   SSE transport to Fireworks' Responses API endpoint. WebSocket is not
    ///   supported by Fireworks.
    ///
    /// - **`openai`** (default when `OPENAI_API_KEY` is set without
    ///   `FIREWORKS_API_KEY`): Uses the built-in OpenAI provider with
    ///   WebSocket support enabled. Works with OpenAI directly or any
    ///   WebSocket-compatible proxy.
    pub fn codex(codex_path: &str, _worktree: &Path, shared: &Path) -> Self {
        let binary = if codex_path.is_empty() {
            "codex"
        } else {
            codex_path
        };

        // Determine which provider to use based on env vars.
        // Priority: CODEX_PROVIDER > FIREWORKS_API_KEY present > OPENAI_API_KEY present
        let provider = detect_codex_provider();

        let (api_key_env, model_env) = match provider {
            CodexProvider::Fireworks => ("FIREWORKS_API_KEY", "FIREWORKS_MODEL"),
            CodexProvider::OpenAI => ("OPENAI_API_KEY", "OPENAI_MODEL"),
        };

        Self {
            binary_path: PathBuf::from(binary),
            // FORGE needs network access (git push, GitHub API) to push changes and
            // create PRs directly. The `danger-full-access` sandbox mode allows this
            // while still providing filesystem write access. Network domains are
            // restricted via the permissions TOML (api.github.com, *.github.com only).
            // The host-side push_and_create_pr() remains as a deterministic fallback
            // if FORGE's push fails for any reason.
            base_flags: vec![
                "exec".into(),
                "--sandbox".into(),
                "danger-full-access".into(),
            ],
            forge_flags: vec![],
            forge_pr_flags: vec![],
            sentinel_flags: vec!["--json".into(), "--ephemeral".into()],
            api_key_env: api_key_env.into(),
            base_url_env: Some("OPENAI_BASE_URL".into()),
            model_env: Some(model_env.into()),
            home_env_var: Some("CODEX_HOME".into()),
            home_dir_suffix: ".codex-home".into(),
            plugin_dir_rel: PathBuf::from(".agents")
                .join("plugins")
                .join("orchestration"),
            settings_rel: PathBuf::from(".codex").join("config.toml"),
            uses_stdin_prompt: true,
            mcp_config_rel: PathBuf::from(".codex").join("config.toml"),
            needs_extras_provisioning: true,
            // The shared directory (.pair-shared/) now lives inside the
            // worktree, so it is already writable under the workspace-write
            // sandbox — no --add-dir needed.  (The --add-dir flag has a
            // known bug in Codex v0.130.0 where it reports the path as
            // writable but does not create the bind mount, causing
            // EROFS errors at runtime.)
            forge_extra_args: vec![],
            // SENTINEL's CWD is the shared directory (.pair-shared/ inside
            // the worktree) so it loads its own .codex/config.toml with
            // read-only evaluation settings.  The workspace-write sandbox
            // allows reads through the read-only root mount, so SENTINEL
            // can evaluate source code in the worktree parent directory
            // without needing --add-dir.  Writes go to the CWD (shared
            // dir) which is inside the writable workspace.
            // --skip-git-repo-check is needed because .pair-shared/ is
            // not a git repository root.
            sentinel_extra_args: vec![
                "-C".into(),
                shared.to_string_lossy().to_string(),
                "--skip-git-repo-check".into(),
            ],
        }
    }

    /// Get the backend-specific home directory path.
    pub fn home_dir(&self, base: &Path) -> PathBuf {
        if self.home_dir_suffix.is_empty() {
            base.to_path_buf()
        } else {
            base.join(&self.home_dir_suffix)
        }
    }

    /// Get the settings file absolute path.
    pub fn settings_path(&self, target: &Path) -> PathBuf {
        target.join(&self.settings_rel)
    }

    /// Get the plugin directory absolute path.
    pub fn plugin_dir(&self, target: &Path) -> PathBuf {
        target.join(&self.plugin_dir_rel)
    }

    /// Get the MCP config file absolute path.
    pub fn mcp_config_path(&self, target: &Path) -> PathBuf {
        target.join(&self.mcp_config_rel)
    }
}

/// Get a BackendConfig for the given CliBackend type.
/// This is the single dispatch point — all backend-specific values flow through here.
pub fn get_backend_config(backend: CliBackend, worktree: &Path, shared: &Path) -> BackendConfig {
    match backend {
        CliBackend::Claude => {
            let path = std::env::var("CLAUDE_PATH").unwrap_or_else(|_| "claude".to_string());
            BackendConfig::claude(&path, worktree, shared)
        }
        CliBackend::Codex => {
            let path = std::env::var("CODEX_PATH").unwrap_or_else(|_| "codex".to_string());
            BackendConfig::codex(&path, worktree, shared)
        }
        CliBackend::Aider | CliBackend::Goose => {
            // Aider and Goose follow the same Claude-style config layout
            let binary = backend.binary_name();
            let path = std::env::var(backend.path_env_var()).unwrap_or_else(|_| binary.to_string());
            BackendConfig::claude(&path, worktree, shared)
        }
    }
}

/// Codex model provider selection.
///
/// Determines how codex CLI routes API requests:
/// - `Fireworks`: Custom provider with SSE transport (no WebSocket), for Fireworks AI.
/// - `OpenAI`: Provider mode determined by endpoint probing — Responses API if
///   the endpoint supports `/v1/responses`, otherwise Chat Completions.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CodexProvider {
    /// Fireworks AI — Responses API over SSE (no WebSocket support).
    Fireworks,
    /// OpenAI — mode determined by endpoint capability probing.
    OpenAI,
}

/// Whether an OpenAI-compatible endpoint supports the Responses API (`/v1/responses`).
///
/// Determined at runtime by probing the endpoint. Cached for the process lifetime
/// so we only probe once per startup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EndpointMode {
    /// Endpoint supports `/v1/responses` — use the Responses API with custom provider config.
    ResponsesApi,
    /// Endpoint only supports `/v1/chat/completions` — start a local Responses→ChatCompletions
    /// proxy since Codex CLI v0.133.0+ requires `wire_api="responses"`. The proxy translates
    /// `/v1/responses` requests to `/v1/chat/completions` requests.
    ChatCompletions,
}

impl CodexProvider {
    /// Get the provider ID string used in codex config (e.g. "fireworks" or "openai").
    pub fn as_str(&self) -> &'static str {
        match self {
            CodexProvider::Fireworks => "fireworks",
            CodexProvider::OpenAI => "openai",
        }
    }
}

/// Detect which codex model provider to use based on environment.
///
/// Selection priority:
/// 1. `CODEX_PROVIDER` env var (explicit: `"fireworks"` or `"openai"`)
/// 2. `FIREWORKS_API_KEY` present → Fireworks (SSE transport, no WebSocket)
/// 3. Otherwise → OpenAI (mode determined by endpoint probing)
fn detect_codex_provider() -> CodexProvider {
    // Explicit override takes priority
    if let Ok(provider) = std::env::var("CODEX_PROVIDER") {
        match provider.to_lowercase().as_str() {
            "fireworks" => {
                info!("codex: provider forced to fireworks via CODEX_PROVIDER");
                return CodexProvider::Fireworks;
            }
            "openai" => {
                info!("codex: provider forced to openai via CODEX_PROVIDER");
                return CodexProvider::OpenAI;
            }
            other => {
                warn!(
                    provider = other,
                    "Unknown CODEX_PROVIDER value, falling back to auto-detection"
                );
            }
        }
    }

    // Auto-detect: if FIREWORKS_API_KEY is set, use Fireworks provider
    // (Fireworks doesn't support WebSocket, so we must use SSE transport).
    // Otherwise, use the built-in OpenAI provider which supports WebSocket.
    if std::env::var("FIREWORKS_API_KEY")
        .ok()
        .filter(|k| !k.trim().is_empty())
        .is_some()
    {
        info!("codex: auto-detected Fireworks provider (FIREWORKS_API_KEY present)");
        CodexProvider::Fireworks
    } else {
        info!("codex: using OpenAI-compatible provider");
        CodexProvider::OpenAI
    }
}

/// Probe whether the OpenAI-compatible endpoint (OPENAI_BASE_URL) supports the
/// Responses API (`/v1/responses`).
///
/// Sends a lightweight POST request to `{base_url}/responses` with a minimal payload.
/// If the endpoint returns 2xx or a 4xx that indicates the route exists (e.g., 401,
/// 422), we consider it Responses API–capable. If it returns 404, the route doesn't
/// exist and we fall back to Chat Completions mode.
///
/// Results are cached for the process lifetime via a `OnceLock`.
///
/// NOTE: The HTTP probe runs in a dedicated OS thread to avoid Tokio runtime
/// conflicts — `reqwest::blocking` creates its own Tokio runtime internally,
/// which panics if called from within an existing async runtime context.
///
/// Although `tokio::task::spawn_blocking` would be preferable to avoid
/// blocking a Tokio worker thread, this function is called from synchronous
/// command-construction code (`build_cli_command`/`build_sentinel_command`).
/// Converting to async would require a broader refactor. The impact is
/// mitigated by `OnceLock` caching — the probe runs at most once per process,
/// and the 5s timeout bounds the worst-case blocking duration.
fn probe_endpoint_supports_responses() -> EndpointMode {
    use std::sync::OnceLock;
    static CACHE: OnceLock<EndpointMode> = OnceLock::new();

    *CACHE.get_or_init(|| {
        let base_url = match std::env::var("OPENAI_BASE_URL") {
            Ok(url) if !url.is_empty() => url.trim_end_matches('/').to_string(),
            _ => {
                info!("codex: no OPENAI_BASE_URL set; assuming Responses API capable");
                return EndpointMode::ResponsesApi;
            }
        };

        let probe_url = format!("{}/responses", base_url);

        // Build a minimal request body that is valid for Responses API probes.
        let body = r#"{"model":"probe","input":"ping"}"#.to_string();
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();

        // Run the HTTP probe in a dedicated OS thread. This is necessary because:
        // - reqwest::blocking creates its own Tokio runtime internally
        // - This function is called from within an async Tokio context
        //   (during ForgeSentinelPair::new → ProcessManager construction →
        //    build_cli_command), and creating a nested runtime panics.
        // - std::thread::spawn creates a new OS thread where the blocking
        //   client can safely create its own runtime.
        let handle = std::thread::spawn(move || {
            // Use a short timeout — this runs during process spawn so must be fast.
            let client = match reqwest::blocking::Client::builder()
                .timeout(std::time::Duration::from_secs(5))
                .build()
            {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "codex: failed to build HTTP client for endpoint probe; assuming Responses API");
                    return EndpointMode::ResponsesApi;
                }
            };

            let result = client
                .post(&probe_url)
                .header("Content-Type", "application/json")
                .header("Authorization", format!("Bearer {}", api_key))
                .body(body)
                .send();

            match result {
                Ok(resp) => {
                    let status = resp.status().as_u16();
                    if status == 404 {
                        // 404 = the /v1/responses route does not exist
                        info!(
                            base_url = %base_url,
                            status,
                            "codex: endpoint does NOT support /v1/responses (404); using Chat Completions mode"
                        );
                        EndpointMode::ChatCompletions
                    } else if (200..300).contains(&status) {
                        // 2xx = the route exists and processed the request (possibly with
                        // an error in the body, but the route is there)
                        info!(
                            base_url = %base_url,
                            status,
                            "codex: endpoint supports /v1/responses (2xx); using Responses API mode"
                        );
                        EndpointMode::ResponsesApi
                    } else if status == 401 || status == 422 || status == 429 {
                        // 401 Unauthorized, 422 Unprocessable, 429 Rate Limited
                        // These all indicate the route EXISTS but the request was invalid
                        // (bad auth, bad payload, rate limit). Safer to try ResponsesApi.
                        info!(
                            base_url = %base_url,
                            status,
                            "codex: endpoint appears to support /v1/responses (auth/validation error); using Responses API mode"
                        );
                        EndpointMode::ResponsesApi
                    } else {
                        // 403 Forbidden, 5xx errors, and anything else — these indicate
                        // the gateway may NOT support /v1/responses (e.g., 403 from
                        // gateways that reject WebSocket upgrades, or 500 from misconfigured
                        // gateways). Default to ChatCompletions mode which uses the proxy,
                        // since it's the safer fallback (the proxy can always translate).
                        warn!(
                            base_url = %base_url,
                            status,
                            "codex: endpoint returned {}; defaulting to Chat Completions mode (proxy will be started)",
                            status
                        );
                        EndpointMode::ChatCompletions
                    }
                }
                Err(e) => {
                    // Network errors (DNS, timeout, connection refused) — default to
                    // ChatCompletions mode since we can't confirm ResponsesApi support.
                    // The local proxy can translate if needed.
                    warn!(
                        error = %e,
                        "codex: endpoint probe failed; defaulting to Chat Completions mode (proxy will be started)"
                    );
                    EndpointMode::ChatCompletions
                }
            }
        });

        match handle.join() {
            Ok(mode) => mode,
            Err(_) => {
                warn!("codex: endpoint probe thread panicked; assuming Responses API capable");
                EndpointMode::ResponsesApi
            }
        }
    })
}

/// Determine whether to use SSE (custom provider) mode for OpenAI-compatible endpoints.
///
/// Instead of relying on a manual env var toggle, we probe the endpoint at runtime:
/// - If the endpoint supports `/v1/responses` → use Responses API (custom provider + SSE)
/// - If the endpoint returns 404 for `/v1/responses` → start a local proxy that translates
///   Responses API requests to Chat Completions format (since Codex CLI only supports
///   `wire_api="responses"`)
///
pub(crate) use agent_client::strip_provider_prefix;

pub fn codex_use_sse() -> bool {
    // NOTE: The former CODEX_USE_SSE env var has been removed. This function
    // now probes the endpoint capability instead of reading a static flag.
    probe_endpoint_supports_responses() == EndpointMode::ChatCompletions
}

/// Append `--disable` flags for Codex features that register non-function tool
/// types in the Responses API.
///
/// Non-OpenAI providers (proxies, custom endpoints) typically only support the
/// `"function"` tool type in the OpenAI Responses API.  Codex features like
/// computer_use, MCP, browser_use, etc. register tools with other type values
/// (`"computer_preview"`, `"mcp"`, etc.) which cause a 400 "unknown tool type"
/// error from these providers.
///
/// This function disables all known Codex features that produce non-function
/// tool types, leaving only `"function"` tools (shell, file ops, etc.) which
/// are universally supported.
///
/// Must be called **in addition** to removing MCP server entries from config.toml
/// when using a custom SSE provider, because MCP tools also use the `"mcp"` type.
fn append_sse_disable_flags(cmd: &mut Command) {
    // Original set — always needed for SSE custom providers:
    cmd.arg("--disable").arg("computer_use");
    cmd.arg("--disable").arg("browser_use");
    cmd.arg("--disable").arg("browser_use_external");
    cmd.arg("--disable").arg("image_generation");
    cmd.arg("--disable").arg("tool_call_mcp_elicitation");
    cmd.arg("--disable").arg("in_app_browser");
    cmd.arg("--disable").arg("tool_suggest");

    // Extended set — additional features that register non-function tool types
    // in the Responses API request.  These caused 400 "unknown tool type" errors
    // on OpenAI-compatible proxies (e.g. api.ai.example.com) that only
    // accept type="function".
    cmd.arg("--disable").arg("apps");
    cmd.arg("--disable").arg("multi_agent");
    cmd.arg("--disable").arg("plugins");
    cmd.arg("--disable").arg("plugin_hooks");
    cmd.arg("--disable").arg("plugin_sharing");
    cmd.arg("--disable").arg("skill_mcp_dependency_install");
    cmd.arg("--disable").arg("goals");
    cmd.arg("--disable").arg("guardian_approval");
    cmd.arg("--disable").arg("workspace_dependencies");
}

/// Configure Codex CLI to use the local Responses→ChatCompletions proxy.
///
/// This is only needed when the upstream gateway supports `/v1/chat/completions`
/// but NOT `/v1/responses`. Since Codex CLI v0.133.0+ only supports
/// `wire_api="responses"`, we start a local proxy that:
///
/// 1. Receives Responses API `POST /v1/responses` requests from Codex
/// 2. Translates them to Chat Completions `POST /v1/chat/completions` requests
/// 3. Forwards to the upstream gateway
/// 4. Translates responses back to Responses API format
///
/// This is a **special-case adapter** — it is NOT used when the gateway
/// natively supports `/v1/responses` (e.g., api.openai.com, Fireworks).
fn configure_responses_proxy(cmd: &mut Command, proxy_url: &str) {
    cmd.arg("-c").arg("model_provider=\"responses_proxy\"");
    cmd.arg("-c")
        .arg("model_providers.responses_proxy.name=\"ResponsesProxy\"");
    cmd.arg("-c").arg(format!(
        "model_providers.responses_proxy.base_url=\"{}\"",
        proxy_url.trim_end_matches('/')
    ));
    cmd.arg("-c")
        .arg("model_providers.responses_proxy.env_key=\"OPENAI_API_KEY\"");
    cmd.arg("-c")
        .arg("model_providers.responses_proxy.wire_api=\"responses\"");
    cmd.arg("-c")
        .arg("model_providers.responses_proxy.supports_websockets=false");
    // Disable Responses API tool types that non-OpenAI providers don't support.
    append_sse_disable_flags(cmd);
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

/// Structured output from Codex exec --json.
/// Codex emits a JSON array of turn objects, each containing items.
#[derive(Debug, Clone, Deserialize)]
pub struct CodexExecResult {
    /// Thread identifier for this execution session
    pub thread_id: Option<String>,
    /// All turns in the conversation
    pub turns: Vec<CodexTurn>,
    /// Extracted result text (if any)
    pub result_text: Option<String>,
    /// Whether execution completed successfully
    pub success: bool,
}

/// A single turn in the Codex conversation.
#[derive(Debug, Clone, Deserialize)]
pub struct CodexTurn {
    /// Turn number (0-indexed)
    pub n: Option<u32>,
    /// Items produced in this turn
    #[serde(default)]
    pub items: Vec<CodexItem>,
}

/// An item within a Codex turn (tool use, assistant message, etc.)
#[derive(Debug, Clone, Deserialize)]
pub struct CodexItem {
    /// Type of item (e.g., "tool_call", "tool_result", "message")
    #[serde(rename = "type")]
    pub item_type: Option<String>,
    /// Tool name if this is a tool call
    pub name: Option<String>,
    /// Content of the item
    pub content: Option<String>,
    /// Output from tool execution
    pub output: Option<String>,
}

/// Parse Codex exec --json output into structured result.
///
/// Codex exec with --json emits an array of turn objects. Each turn contains
/// an array of items (tool calls, results, messages). We extract the final
/// result text from the last assistant message or tool result.
pub fn parse_codex_exec_output(raw: &str) -> Result<CodexExecResult> {
    if raw.trim().is_empty() {
        return Ok(CodexExecResult {
            thread_id: None,
            turns: vec![],
            result_text: None,
            success: false,
        });
    }

    // Try parsing as full exec result with thread_id first (most specific)
    if let Ok(full_result) = serde_json::from_str::<serde_json::Value>(raw) {
        if full_result.get("thread_id").is_some() || full_result.get("turns").is_some() {
            let thread_id = full_result
                .get("thread_id")
                .and_then(|v| v.as_str())
                .map(String::from);

            let turns = full_result
                .get("turns")
                .and_then(|v| serde_json::from_value::<Vec<CodexTurn>>(v.clone()).ok())
                .unwrap_or_default();

            return Ok(extract_result_from_turns_with_thread(turns, thread_id));
        }
    }

    // Try parsing as JSON array of turns
    if let Ok(turns) = serde_json::from_str::<Vec<CodexTurn>>(raw) {
        return Ok(extract_result_from_turns(turns));
    }

    // Try parsing as single turn object
    if let Ok(turn) = serde_json::from_str::<CodexTurn>(raw) {
        return Ok(extract_result_from_turns(vec![turn]));
    }

    // Not valid JSON - treat as raw text output
    Ok(CodexExecResult {
        thread_id: None,
        turns: vec![],
        result_text: Some(raw.to_string()),
        success: true,
    })
}

fn extract_result_from_turns(turns: Vec<CodexTurn>) -> CodexExecResult {
    extract_result_from_turns_with_thread(turns, None)
}

fn extract_result_from_turns_with_thread(
    turns: Vec<CodexTurn>,
    thread_id: Option<String>,
) -> CodexExecResult {
    let success = !turns.is_empty();

    // Extract result text from the last turn's last item
    let result_text = turns
        .last()
        .and_then(|turn| turn.items.last())
        .and_then(|item| {
            // Prefer content from tool results
            if item.item_type.as_deref() == Some("tool_result") {
                item.output.clone().or_else(|| item.content.clone())
            } else if item.item_type.as_deref() == Some("message") {
                item.content.clone()
            } else {
                item.content.clone().or_else(|| item.output.clone())
            }
        });

    CodexExecResult {
        thread_id,
        turns,
        result_text,
        success,
    }
}

/// Wraps a Tokio runtime so it can be dropped safely from an async context.
///
/// Dropping a multi-thread runtime blocks the current thread by joining worker
/// threads, which panics inside an async task. This wrapper offloads the drop
/// to a dedicated OS thread.
pub struct ThreadSafeRuntime(Option<tokio::runtime::Runtime>);

impl ThreadSafeRuntime {
    fn new(rt: tokio::runtime::Runtime) -> Self {
        Self(Some(rt))
    }
}

impl Drop for ThreadSafeRuntime {
    fn drop(&mut self) {
        if let Some(rt) = self.0.take() {
            std::thread::spawn(move || {
                drop(rt);
            });
        }
    }
}

/// Holds the initialized state for the responses proxy. The address and runtime
/// are always set atomically together, preventing a race where the cached address
/// points to a proxy whose runtime has been dropped.
#[allow(dead_code)]
struct ResponsesProxyState {
    /// The proxy base URL (e.g., "http://127.0.0.1:35173").
    addr: String,
    /// The Tokio runtime that drives the proxy server. Kept alive for the
    /// lifetime of ProcessManager so the proxy continues serving requests.
    runtime: ThreadSafeRuntime,
    /// Shutdown sender for graceful proxy termination. When dropped, the
    /// server stops accepting new connections and drains in-flight requests.
    shutdown_tx: tokio::sync::watch::Sender<bool>,
}

/// Manages FORGE and SENTINEL processes.
/// Supports multiple CLI backends via the `BackendConfig` abstraction — adding a new backend
/// only requires creating a new `BackendConfig` and registering it.
pub struct ProcessManager {
    /// Registry of backend configs, keyed by CliBackend
    backends: HashMap<CliBackend, BackendConfig>,
    /// Default CLI backend to use
    default_backend: CliBackend,
    /// Model backend override from registry.json (e.g., "deepseek-v4-flash").
    /// When set, this overrides the OPENAI_MODEL / ANTHROPIC_MODEL env var
    /// for the spawned CLI process.
    model_backend: Option<String>,
    github_token: String,
    redis_url: Option<String>,
    proxy_url: Option<String>,
    proxy_api_key: Option<String>,
    /// Local Responses API proxy state (started when endpoint doesn't
    /// support /v1/responses). When set, Codex CLI is configured to point
    /// at this proxy instead of the upstream directly.
    /// Uses a single Mutex to ensure atomic initialization — the address
    /// and runtime are always set together, preventing a race where the
    /// address points to a killed proxy.
    responses_proxy: std::sync::Mutex<Option<ResponsesProxyState>>,
    /// Workspace provider mode for determining how to spawn agents.
    workspace_provider: crate::types::WorkspaceProvider,
    /// Coder workspace ID when workspace_provider is Coder.
    coder_workspace_id: Option<String>,
    /// Coder client for workspace execution.
    #[cfg(feature = "coder")]
    coder_client: Option<coder_client::CoderClient>,
}

impl ProcessManager {
    /// Create a new ProcessManager with default CLI backend (Claude).
    pub fn new(github_token: impl Into<String>, worktree: &Path, shared: &Path) -> Self {
        let proxy_url = std::env::var("PROXY_URL").ok();
        let proxy_api_key = std::env::var("PROXY_API_KEY").ok();

        let mut backends = HashMap::new();
        backends.insert(
            CliBackend::Claude,
            BackendConfig::claude(
                &std::env::var("CLAUDE_PATH").unwrap_or_else(|_| "claude".to_string()),
                worktree,
                shared,
            ),
        );
        backends.insert(
            CliBackend::Codex,
            BackendConfig::codex(
                &std::env::var("CODEX_PATH").unwrap_or_else(|_| "codex".to_string()),
                worktree,
                shared,
            ),
        );

        // Validate all registered backends (logs warnings; spawn-time
        // validation will fail hard in Local mode if binary is missing).
        // At construction time the provider is always Local; when
        // with_coder_config() is called later the provider changes to
        // Coder and the binaries are no longer needed on the host.
        let initial_provider = crate::types::WorkspaceProvider::Local;
        for (backend, config) in &backends {
            if let Err(e) = Self::validate_cli_binary(
                &config.binary_path,
                backend.binary_name(),
                &initial_provider,
            ) {
                warn!("{}", e);
            }
        }

        Self {
            backends,
            default_backend: CliBackend::default(),
            model_backend: None,
            github_token: github_token.into(),
            redis_url: None,
            proxy_url,
            proxy_api_key,
            responses_proxy: std::sync::Mutex::new(None),
            workspace_provider: crate::types::WorkspaceProvider::Local,
            coder_workspace_id: None,
            #[cfg(feature = "coder")]
            coder_client: None,
        }
    }

    /// Create a ProcessManager with Redis backend.
    pub fn with_redis(
        github_token: impl Into<String>,
        redis_url: impl Into<String>,
        worktree: &Path,
        shared: &Path,
    ) -> Self {
        let proxy_url = std::env::var("PROXY_URL").ok();
        let proxy_api_key = std::env::var("PROXY_API_KEY").ok();

        let mut backends = HashMap::new();
        backends.insert(
            CliBackend::Claude,
            BackendConfig::claude(
                &std::env::var("CLAUDE_PATH").unwrap_or_else(|_| "claude".to_string()),
                worktree,
                shared,
            ),
        );
        backends.insert(
            CliBackend::Codex,
            BackendConfig::codex(
                &std::env::var("CODEX_PATH").unwrap_or_else(|_| "codex".to_string()),
                worktree,
                shared,
            ),
        );

        let initial_provider = crate::types::WorkspaceProvider::Local;
        for (backend, config) in &backends {
            if let Err(e) = Self::validate_cli_binary(
                &config.binary_path,
                backend.binary_name(),
                &initial_provider,
            ) {
                warn!("{}", e);
            }
        }

        Self {
            backends,
            default_backend: CliBackend::default(),
            model_backend: None,
            github_token: github_token.into(),
            redis_url: Some(redis_url.into()),
            proxy_url,
            proxy_api_key,
            responses_proxy: std::sync::Mutex::new(None),
            workspace_provider: crate::types::WorkspaceProvider::Local,
            coder_workspace_id: None,
            #[cfg(feature = "coder")]
            coder_client: None,
        }
    }

    /// Create a ProcessManager with proxy configuration.
    pub fn with_proxy(
        github_token: impl Into<String>,
        redis_url: Option<String>,
        proxy_url: impl Into<String>,
        worktree: &Path,
        shared: &Path,
    ) -> Self {
        let proxy_api_key = std::env::var("PROXY_API_KEY").ok();

        let mut backends = HashMap::new();
        backends.insert(
            CliBackend::Claude,
            BackendConfig::claude(
                &std::env::var("CLAUDE_PATH").unwrap_or_else(|_| "claude".to_string()),
                worktree,
                shared,
            ),
        );
        backends.insert(
            CliBackend::Codex,
            BackendConfig::codex(
                &std::env::var("CODEX_PATH").unwrap_or_else(|_| "codex".to_string()),
                worktree,
                shared,
            ),
        );

        let initial_provider = crate::types::WorkspaceProvider::Local;
        for (backend, config) in &backends {
            if let Err(e) = Self::validate_cli_binary(
                &config.binary_path,
                backend.binary_name(),
                &initial_provider,
            ) {
                warn!("{}", e);
            }
        }

        Self {
            backends,
            default_backend: CliBackend::default(),
            model_backend: None,
            github_token: github_token.into(),
            redis_url,
            proxy_url: Some(proxy_url.into()),
            proxy_api_key,
            responses_proxy: std::sync::Mutex::new(None),
            workspace_provider: crate::types::WorkspaceProvider::Local,
            coder_workspace_id: None,
            #[cfg(feature = "coder")]
            coder_client: None,
        }
    }

    /// Set the default CLI backend.
    pub fn with_default_backend(mut self, backend: CliBackend) -> Self {
        self.default_backend = backend;
        self
    }

    /// Set the model backend override (from registry.json's model_backend field).
    /// When set, this overrides the OPENAI_MODEL / ANTHROPIC_MODEL env var
    /// for the spawned CLI process.
    pub fn with_model_backend(mut self, model: Option<String>) -> Self {
        self.model_backend = model.filter(|m| !m.is_empty());
        self
    }

    /// Configure Coder workspace execution.
    /// When workspace_provider is Coder, spawn methods will use the Coder
    /// exec API instead of local process spawning.
    pub fn with_coder_config(
        mut self,
        coder_url: &str,
        coder_api_token: &str,
        coder_workspace_id: &str,
    ) -> Self {
        self.workspace_provider = crate::types::WorkspaceProvider::Coder;
        self.coder_workspace_id = Some(coder_workspace_id.to_string());
        #[cfg(not(feature = "coder"))]
        let _ = (coder_url, coder_api_token);
        #[cfg(feature = "coder")]
        {
            let session_token = std::env::var("CODER_SESSION_TOKEN")
                .unwrap_or_else(|_| coder_api_token.to_string());
            let client = coder_client::CoderClient::new(coder_url, coder_api_token)
                .with_workspace_name(coder_workspace_id)
                .with_session_token(&session_token);
            self.coder_client = Some(client);
        }
        self
    }

    /// Ensure the local Responses API proxy is running. Starts it if not yet
    /// started, and returns the proxy base URL (e.g., "http://127.0.0.1:PORT").
    ///
    /// The proxy translates `/v1/responses` (Responses API) requests into
    /// `/v1/chat/completions` (Chat Completions API) requests and forwards
    /// them to the upstream gateway. This is needed because Codex CLI v0.133.0+
    /// only supports `wire_api="responses"` but many OpenAI-compatible gateways
    /// only implement `/v1/chat/completions`.
    pub fn ensure_responses_proxy(&self) -> Result<String> {
        // Check if already started under the lock to prevent races.
        // If two threads race here, only one will start the proxy; the other
        // will find it already initialized and return the cached address.
        {
            let guard = self
                .responses_proxy
                .lock()
                .expect("responses_proxy mutex poisoned");
            if let Some(ref state) = *guard {
                return Ok(state.addr.clone());
            }
        }

        let upstream_base_url = std::env::var("OPENAI_BASE_URL")
            .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
        let api_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();

        if api_key.is_empty() {
            return Err(anyhow::anyhow!(
                "OPENAI_API_KEY is required for the responses proxy (endpoint does not support /v1/responses)"
            ));
        }

        info!(
            upstream = %upstream_base_url,
            "responses_proxy: starting Responses→ChatCompletions proxy for Codex CLI"
        );

        let upstream_clone = upstream_base_url.clone();
        let api_key_clone = api_key.clone();

        // Run the proxy startup in a dedicated OS thread. This is necessary because:
        // - Creating a new multi-thread Tokio runtime panics if called from within
        //   an existing Tokio runtime (which happens when build_cli_command or
        //   build_sentinel_command is invoked from async spawn functions).
        // - We also need block_on on the new runtime to start the proxy.
        // - std::thread::spawn creates a new OS thread where the runtime can be
        //   safely created and kept alive.
        let handle = std::thread::spawn(
            move || -> Result<(String, ThreadSafeRuntime, tokio::sync::watch::Sender<bool>)> {
                let rt = tokio::runtime::Builder::new_multi_thread()
                    .worker_threads(2)
                    .enable_all()
                    .build()
                    .context("Failed to create Tokio runtime for responses proxy")?;

                let (addr, shutdown_tx) = rt.block_on(async {
                    crate::responses_proxy::start_responses_proxy(upstream_clone, api_key_clone)
                        .await
                })?;

                let proxy_url = format!("http://{}:{}", addr.ip(), addr.port());
                Ok((proxy_url, ThreadSafeRuntime::new(rt), shutdown_tx))
            },
        );

        let (proxy_url, rt, shutdown_tx) = match handle.join() {
            Ok(result) => result.context("responses_proxy: failed to start")?,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "responses_proxy: dedicated thread panicked: {:?}",
                    e
                ));
            }
        };

        info!(proxy_url = %proxy_url, "responses_proxy: proxy started");

        // Store both the address and runtime atomically under the same lock.
        // This prevents a race where the address is cached but the runtime
        // is from a different (killed) proxy instance.
        let mut guard = self
            .responses_proxy
            .lock()
            .expect("responses_proxy mutex poisoned");
        // Another thread may have initialized the proxy while we were starting ours.
        // If so, return the cached address and drop our duplicate runtime.
        if let Some(ref state) = *guard {
            return Ok(state.addr.clone());
        }
        *guard = Some(ResponsesProxyState {
            addr: proxy_url.clone(),
            runtime: rt,
            shutdown_tx,
        });

        Ok(proxy_url)
    }

    /// Register a custom backend config (for testing or third-party backends).
    pub fn register_backend(&mut self, backend: CliBackend, config: BackendConfig) {
        if let Err(e) = Self::validate_cli_binary(
            &config.binary_path,
            backend.binary_name(),
            &self.workspace_provider,
        ) {
            warn!("{}", e);
        }
        self.backends.insert(backend, config);
    }

    /// Get the backend config for a given type.
    pub fn get_backend(&self, backend: CliBackend) -> &BackendConfig {
        self.backends.get(&backend).unwrap_or_else(|| {
            // Fallback: build a default config on the fly
            panic!("Backend {:?} not registered", backend);
        })
    }

    /// Validate a CLI binary exists and is executable.
    ///
    /// In Coder mode (workspace_provider == Coder), the CLI runs inside the
    /// workspace provided by the Coder module — it is NOT required on the
    /// host.  Validation is skipped entirely in that case to avoid false
    /// failures (e.g., `claude` not installed locally).
    ///
    /// In Local mode, validation is strict: a missing binary returns `Err`
    /// so the caller can fail fast instead of waiting until spawn time to
    /// discover the problem.
    fn validate_cli_binary(
        path: &Path,
        name: &str,
        workspace_provider: &crate::types::WorkspaceProvider,
    ) -> Result<(), String> {
        // Skip validation when execution happens inside a Coder workspace.
        if matches!(workspace_provider, crate::types::WorkspaceProvider::Coder) {
            info!(
                binary = %path.display(),
                name,
                "Skipping local CLI binary validation — execution targets a Coder workspace"
            );
            return Ok(());
        }

        let env_var = format!("{}_PATH", name.to_uppercase());
        if path.is_absolute() {
            if !path.exists() {
                return Err(format!(
                    "{} binary not found at {}. Install {} CLI or set {} in .env",
                    env_var,
                    path.display(),
                    name,
                    env_var
                ));
            } else {
                #[cfg(unix)]
                {
                    use std::os::unix::fs::PermissionsExt;
                    if let Ok(metadata) = path.metadata() {
                        let perms = metadata.permissions();
                        if perms.mode() & 0o111 == 0 {
                            return Err(format!(
                                "{} binary exists at {} but is not executable. Run: chmod +x {}",
                                env_var,
                                path.display(),
                                path.display()
                            ));
                        }
                    }
                }
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
                    return Err(format!(
                        "{} CLI binary not found on PATH. Install it from {} or set {}_PATH in .env to an absolute path",
                        name, install_url, name.to_uppercase()
                    ));
                }
            }
        }
        Ok(())
    }

    /// Ensure the backend-specific home directory exists and write a minimal
    /// config.toml if needed. For Codex, CODEX_HOME must point to an existing
    /// directory with a valid config.toml, otherwise codex refuses to start.
    fn ensure_home_dir(cmd: &mut Command, config: &BackendConfig, base_dir: &Path) {
        if let Some(home_env_var) = &config.home_env_var {
            let home_dir = config.home_dir(base_dir);
            if let Err(e) = std::fs::create_dir_all(&home_dir) {
                warn!(
                    path = %home_dir.display(),
                    error = %e,
                    "Failed to create {} directory — CLI may fail to start",
                    home_env_var
                );
            }
            // Write a minimal config.toml to the home dir so the CLI has a
            // valid user-layer config. Provider definitions are passed via
            // `-c` flags at spawn time (project-local config cannot set
            // `model_providers` due to codex's security denylist).
            let config_toml = home_dir.join("config.toml");
            if !config_toml.exists() {
                let minimal_config = r#"# Auto-generated by AgentFlow — minimal user config
# Provider config is passed via -c flags at spawn time.
[projects."/tmp"]
trust_level = "trusted"
"#;
                if let Err(e) = std::fs::write(&config_toml, minimal_config) {
                    warn!(
                        path = %config_toml.display(),
                        error = %e,
                        "Failed to write minimal config.toml"
                    );
                }
            }
            cmd.env(home_env_var, home_dir.to_string_lossy().to_string());
            debug!(home_dir = %home_dir.display(), "Set {} for isolated config", home_env_var);
        }
    }

    fn inject_proxy_env(
        cmd: &mut Command,
        backend: &BackendConfig,
        proxy_url: &str,
        proxy_api_key: Option<&str>,
    ) {
        let base_url = proxy_url.trim_end_matches("/v1").trim_end_matches('/');
        if let Some(env_name) = &backend.base_url_env {
            cmd.env(env_name, base_url);
        }
        cmd.env(&backend.api_key_env, proxy_api_key.unwrap_or(""));
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

    /// Build a command for the given CLI backend.
    fn build_cli_command(&self, backend: CliBackend, _worktree: &Path, _shared: &Path) -> Command {
        let config = self.get_backend(backend);
        let mut cmd = Command::new(&config.binary_path);

        for arg in &config.base_flags {
            cmd.arg(arg);
        }

        // Pass model from ProcessManager's model_backend override (from registry.json)
        // or fall back to the backend-specific env var (OPENAI_MODEL, ANTHROPIC_MODEL, etc.)
        // Provider prefixes (e.g. "anthropic/") are stripped because CLI backends
        // expect bare model names.
        if let Some(model) = &self.model_backend {
            if !model.is_empty() {
                let clean = strip_provider_prefix(model);
                cmd.arg("--model").arg(clean);
                info!(model = %model, clean_model = %clean, "{}: using model from registry model_backend", backend.as_str());
            }
        } else if let Some(model_env) = &config.model_env {
            if let Ok(model) = std::env::var(model_env) {
                if !model.is_empty() {
                    let clean = strip_provider_prefix(&model);
                    cmd.arg("--model").arg(clean);
                    info!(model = %model, clean_model = %clean, "{}: using model from {}", backend.as_str(), model_env);
                }
            }
        }

        // Codex CLI provider configuration — determined at startup based on
        // CODEX_PROVIDER env var or auto-detected from available API keys.
        //
        // Three modes:
        //   * Fireworks:          custom provider with supports_websockets=false (SSE only)
        //   * OpenAI + Responses: custom provider using wire_api="responses" + SSE
        //   * OpenAI + Chat:      built-in provider with openai_base_url (Chat Completions fallback)
        //
        // For OpenAI-compatible endpoints that are NOT api.openai.com, we probe the
        // endpoint at startup to determine if it supports the Responses API (/v1/responses).
        // If it does, we use custom SSE provider mode. If it returns 404 for /v1/responses,
        // we fall back to the built-in OpenAI provider which uses Chat Completions.
        //
        // Provider is defined via `-c` runtime flags because `model_providers`
        // is on the project-local config denylist and cannot be set in
        // .codex/config.toml.
        if backend == CliBackend::Codex {
            let provider = detect_codex_provider();
            match provider {
                CodexProvider::Fireworks => {
                    // Select the fireworks provider
                    cmd.arg("-c").arg("model_provider=\"fireworks\"");

                    // Define the fireworks provider: Responses API over SSE, no WebSocket
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.name=\"Fireworks\"");
                    cmd.arg("-c").arg("model_providers.fireworks.base_url=\"https://api.fireworks.ai/inference/v1\"");
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.env_key=\"FIREWORKS_API_KEY\"");
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.wire_api=\"responses\"");
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.supports_websockets=false");
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.requires_openai_auth=false");

                    info!("codex: using Fireworks provider (SSE, no WebSocket)");
                }
                CodexProvider::OpenAI => {
                    // Probe the endpoint to determine if it supports the Responses API.
                    // Endpoints that support /v1/responses get the custom SSE provider;
                    // endpoints that return 404 (like OpenAI-compatible proxies) fall back
                    // to the built-in openai provider with Chat Completions.
                    let endpoint_mode = probe_endpoint_supports_responses();
                    match endpoint_mode {
                        EndpointMode::ResponsesApi => {
                            // Endpoint supports /v1/responses — use custom provider
                            // with SSE transport and disable non-function tool types
                            // that cause errors on non-OpenAI proxies.
                            cmd.arg("-c").arg("model_provider=\"custom\"");
                            cmd.arg("-c").arg("model_providers.custom.name=\"Custom\"");
                            if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
                                if !base_url.is_empty() {
                                    cmd.arg("-c").arg(format!(
                                        "model_providers.custom.base_url=\"{}\"",
                                        base_url.trim_end_matches('/')
                                    ));
                                }
                            }
                            cmd.arg("-c")
                                .arg("model_providers.custom.env_key=\"OPENAI_API_KEY\"");
                            cmd.arg("-c")
                                .arg("model_providers.custom.wire_api=\"responses\"");
                            cmd.arg("-c")
                                .arg("model_providers.custom.supports_websockets=false");
                            // Disable Responses API tool types that non-OpenAI providers
                            // don't support. Without these, Codex only sends "function"
                            // type tools which are universally compatible.
                            // See: https://github.com/openai/codex/discussions/7782
                            append_sse_disable_flags(&mut cmd);
                            info!("codex: using Custom provider with Responses API over SSE (endpoint supports /v1/responses)");
                        }
                        EndpointMode::ChatCompletions => {
                            // SPECIAL CASE: Endpoint does NOT support /v1/responses.
                            //
                            // Codex CLI v0.133.0+ only supports wire_api="responses"
                            // (the Responses API). The built-in `openai` provider always
                            // uses /v1/responses via WebSocket, which fails with 403 on
                            // gateways that don't implement that endpoint.
                            //
                            // This is the format-discrepancy case: the gateway only
                            // speaks Chat Completions but Codex only speaks Responses
                            // API. We bridge this gap with a local proxy that translates
                            // between the two formats.
                            match self.ensure_responses_proxy() {
                                Ok(proxy_url) => {
                                    configure_responses_proxy(&mut cmd, &proxy_url);
                                    info!(proxy_url = %proxy_url, "codex: using local Responses→ChatCompletions proxy (format discrepancy: endpoint lacks /v1/responses)");
                                }
                                Err(e) => {
                                    error!("codex: FAILED to start responses proxy: {}. This endpoint does NOT support /v1/responses and the proxy could not be started. The spawned process will not be able to communicate with the LLM.", e);
                                    // NOTE: We still build the command to avoid changing the
                                    // return type, but it will fail immediately because the
                                    // endpoint doesn't support /v1/responses. The pair watchdog
                                    // will detect the quick exit and mark the pair as blocked.
                                    cmd.arg("-c").arg("model_provider=\"openai\"");
                                    if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
                                        if !base_url.is_empty() {
                                            cmd.arg("-c").arg(format!(
                                                "openai_base_url=\"{}\"",
                                                base_url.trim_end_matches('/')
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Apply backend-specific flags for FORGE mode
        for arg in &config.forge_flags {
            cmd.arg(arg);
        }

        // Apply backend-specific directory settings
        for arg in &config.forge_extra_args {
            cmd.arg(arg);
        }

        cmd
    }

    /// Build a command for SENTINEL mode.
    fn build_sentinel_command(
        &self,
        backend: CliBackend,
        _worktree: &Path,
        _shared: &Path,
    ) -> Command {
        let config = self.get_backend(backend);
        let mut cmd = Command::new(&config.binary_path);

        // SENTINEL uses workspace-write sandbox — it needs to write review files
        // to the shared directory and needs GitHub API access for posting review
        // comments, but should NOT have full filesystem access like FORGE.
        // FORGE uses danger-full-access (set in base_flags) because it needs
        // git push and full filesystem access for code changes.
        cmd.arg("exec");
        cmd.arg("--sandbox");
        cmd.arg("workspace-write");

        // Pass model from ProcessManager's model_backend override (from registry.json)
        // or fall back to the backend-specific env var (OPENAI_MODEL, ANTHROPIC_MODEL, etc.)
        // Provider prefixes (e.g. "anthropic/") are stripped because CLI backends
        // expect bare model names.
        if let Some(model) = &self.model_backend {
            if !model.is_empty() {
                let clean = strip_provider_prefix(model);
                cmd.arg("--model").arg(clean);
                info!(model = %model, clean_model = %clean, "{}: using model from registry model_backend (sentinel)", backend.as_str());
            }
        } else if let Some(model_env) = &config.model_env {
            if let Ok(model) = std::env::var(model_env) {
                if !model.is_empty() {
                    let clean = strip_provider_prefix(&model);
                    cmd.arg("--model").arg(clean);
                    info!(model = %model, clean_model = %clean, "{}: using model from {} (sentinel)", backend.as_str(), model_env);
                }
            }
        }

        // Codex CLI provider configuration (same probe-based logic as FORGE)
        if backend == CliBackend::Codex {
            let provider = detect_codex_provider();
            match provider {
                CodexProvider::Fireworks => {
                    cmd.arg("-c").arg("model_provider=\"fireworks\"");
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.name=\"Fireworks\"");
                    cmd.arg("-c").arg("model_providers.fireworks.base_url=\"https://api.fireworks.ai/inference/v1\"");
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.env_key=\"FIREWORKS_API_KEY\"");
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.wire_api=\"responses\"");
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.supports_websockets=false");
                    cmd.arg("-c")
                        .arg("model_providers.fireworks.requires_openai_auth=false");
                }
                CodexProvider::OpenAI => {
                    let endpoint_mode = probe_endpoint_supports_responses();
                    match endpoint_mode {
                        EndpointMode::ResponsesApi => {
                            cmd.arg("-c").arg("model_provider=\"custom\"");
                            cmd.arg("-c").arg("model_providers.custom.name=\"Custom\"");
                            if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
                                if !base_url.is_empty() {
                                    cmd.arg("-c").arg(format!(
                                        "model_providers.custom.base_url=\"{}\"",
                                        base_url.trim_end_matches('/')
                                    ));
                                }
                            }
                            cmd.arg("-c")
                                .arg("model_providers.custom.env_key=\"OPENAI_API_KEY\"");
                            cmd.arg("-c")
                                .arg("model_providers.custom.wire_api=\"responses\"");
                            cmd.arg("-c")
                                .arg("model_providers.custom.supports_websockets=false");
                            // Disable Responses API tool types that non-OpenAI providers
                            // don't support. See build_cli_command for rationale.
                            append_sse_disable_flags(&mut cmd);
                            info!("codex: sentinel using Custom provider with Responses API over SSE (endpoint supports /v1/responses)");
                        }
                        EndpointMode::ChatCompletions => {
                            // SPECIAL CASE: Same format discrepancy as FORGE.
                            // Endpoint only supports /v1/chat/completions but Codex
                            // requires /v1/responses. Use the local translation proxy.
                            match self.ensure_responses_proxy() {
                                Ok(proxy_url) => {
                                    configure_responses_proxy(&mut cmd, &proxy_url);
                                    info!(proxy_url = %proxy_url, "codex: sentinel using local Responses→ChatCompletions proxy (format discrepancy)");
                                }
                                Err(e) => {
                                    error!("codex: sentinel FAILED to start responses proxy: {}. This endpoint does NOT support /v1/responses and the proxy could not be started. The spawned process will not be able to communicate with the LLM.", e);
                                    cmd.arg("-c").arg("model_provider=\"openai\"");
                                    if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
                                        if !base_url.is_empty() {
                                            cmd.arg("-c").arg(format!(
                                                "openai_base_url=\"{}\"",
                                                base_url.trim_end_matches('/')
                                            ));
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
        }

        // Apply backend-specific sentinel flags
        for arg in &config.sentinel_flags {
            cmd.arg(arg);
        }

        // Apply backend-specific directory settings
        for arg in &config.sentinel_extra_args {
            cmd.arg(arg);
        }

        cmd
    }

    /// Inject environment variables for the CLI backend.
    fn inject_cli_env(&self, cmd: &mut Command, backend: CliBackend) {
        let config = self.get_backend(backend);

        if let Some(proxy_url) = &self.proxy_url {
            Self::inject_proxy_env(cmd, config, proxy_url, self.proxy_api_key.as_deref());
        } else {
            // Pass through backend-specific API key from environment
            cmd.env(
                &config.api_key_env,
                std::env::var(&config.api_key_env).unwrap_or_default(),
            );

            // Pass through both API keys for Codex — the active provider
            // determines which one is actually used for authentication.
            if backend == CliBackend::Codex {
                let provider = detect_codex_provider();
                match provider {
                    CodexProvider::Fireworks => {
                        // Fireworks provider reads FIREWORKS_API_KEY (set via
                        // env_key in the provider config)
                        cmd.env(
                            "FIREWORKS_API_KEY",
                            std::env::var("FIREWORKS_API_KEY").unwrap_or_default(),
                        );
                        // Also set OPENAI_API_KEY for backward compat (some
                        // codex internals may reference it during auth fallback)
                        cmd.env(
                            "OPENAI_API_KEY",
                            std::env::var("OPENAI_API_KEY")
                                .or_else(|_| std::env::var("FIREWORKS_API_KEY"))
                                .unwrap_or_default(),
                        );
                    }
                    CodexProvider::OpenAI => {
                        // OpenAI provider reads OPENAI_API_KEY, but Codex v0.133.0
                        // uses "managed" auth which requires CODEX_API_KEY for the
                        // Authorization header. Without CODEX_API_KEY, Codex detects
                        // OPENAI_API_KEY but doesn't attach it to requests, resulting
                        // in 401 Unauthorized errors.
                        let openai_key = std::env::var("OPENAI_API_KEY").unwrap_or_default();
                        cmd.env("OPENAI_API_KEY", &openai_key);
                        if !openai_key.is_empty() {
                            cmd.env("CODEX_API_KEY", &openai_key);
                        }
                    }
                }
            }

            // For OpenAI-compatible backends, also pass through base URL.
            // When the responses proxy is active, we skip this because Codex
            // points at the local proxy via model_providers.responses_proxy.base_url
            // instead of using OPENAI_BASE_URL directly.
            if self
                .responses_proxy
                .lock()
                .expect("responses_proxy mutex poisoned")
                .is_none()
            {
                if let Some(base_url_env) = &config.base_url_env {
                    if let Ok(base_url) = std::env::var(base_url_env) {
                        cmd.env(base_url_env, base_url);
                    } else if base_url_env == "OPENAI_BASE_URL" {
                        // Also support OPENAI_API_URL for backwards compatibility
                        if let Ok(api_url) = std::env::var("OPENAI_API_URL") {
                            let base_url = api_url
                                .trim_end_matches("/chat/completions")
                                .trim_end_matches("/completions")
                                .trim_end_matches('/');
                            cmd.env("OPENAI_BASE_URL", base_url);
                        }
                    }
                }
            }
        }

        // Common LLM environment variables
        Self::inject_llm_env(cmd);
    }

    #[cfg(feature = "coder")]
    fn coder_context(&self) -> Result<(coder_client::CoderClient, String)> {
        let client = self
            .coder_client
            .clone()
            .context("Coder workspace execution requested but Coder client is not configured")?;
        let workspace_id = self
            .coder_workspace_id
            .clone()
            .context("Coder workspace execution requested but workspace_id is missing")?;
        Ok((client, workspace_id))
    }

    /// Shared env-building for Coder spawns.  Sets the common `SPRINTLESS_*`
    /// variables, injects backend LLM env, ensures a HOME dir, and configures
    /// either Redis (using a *container-reachable* URL, not the host-loopback
    /// one) or a state-file fallback.
    ///
    /// This exists once so FORGE and SENTINEL spawns share an identical env
    /// contract — adding a new variable here reaches both.
    #[cfg(feature = "coder")]
    fn configure_coder_command(
        &self,
        cmd: &mut Command,
        pair_id: &str,
        ticket_id: &str,
        segment: &str,
        worktree: &Path,
        shared: &Path,
        backend: CliBackend,
        extra_envs: &[(&str, &str)],
    ) {
        let config = self.get_backend(backend);
        if config.needs_extras_provisioning {
            let _ = self.apply_codex_extras();
        }

        cmd.env("SPRINTLESS_PAIR_ID", pair_id)
            .env("SPRINTLESS_TICKET_ID", ticket_id)
            .env("SPRINTLESS_SEGMENT", segment)
            .env("SPRINTLESS_WORKTREE", worktree.to_string_lossy().to_string())
            .env("SPRINTLESS_SHARED", shared.to_string_lossy().to_string())
            .env("SPRINTLESS_GITHUB_TOKEN", &self.github_token);
        for (k, v) in extra_envs {
            cmd.env(k, v);
        }
        self.inject_cli_env(cmd, backend);
        Self::ensure_home_dir(cmd, config, worktree);

        if let Some(host_redis_url) = &self.redis_url {
            // The remote process runs inside the Coder container on the
            // compose network; the host's loopback Redis URL is unreachable
            // there, so rewrite it to the container-reachable form.
            cmd.env(
                "SPRINTLESS_REDIS_URL",
                container_reachable_redis_url(host_redis_url),
            );
        } else {
            cmd.env(
                "SPRINTLESS_STATE_FILE",
                shared.join("state.json").to_string_lossy().to_string(),
            );
        }
    }

    #[cfg(feature = "coder")]
    async fn spawn_coder_forge(
        &self,
        pair_id: &str,
        ticket_id: &str,
        worktree: &Path,
        shared: &Path,
        backend: CliBackend,
        pr_mode: bool,
    ) -> Result<ManagedProcess> {
        use std::sync::Arc;

        let (client, workspace_id) = self.coder_context()?;
        let prompt = if pr_mode {
            self.build_forge_pr_prompt(shared)
        } else {
            self.build_forge_prompt(shared)
        };
        let prompt_file = format!("/tmp/agentflow-{}-forge.prompt", pair_id);
        client
            .workspace_write_file(&workspace_id, &prompt_file, &prompt)
            .await
            .context("Failed to write FORGE prompt into Coder workspace")?;

        let mut cmd = self.build_cli_command(backend, worktree, shared);
        self.configure_coder_command(
            &mut cmd,
            pair_id,
            ticket_id,
            "",
            worktree,
            shared,
            backend,
            &[],
        );

        let (env_file, shell_cmd) =
            prepare_coder_remote_command(&client, &workspace_id, pair_id, &cmd).await?;
        let log_path = format!("/tmp/agentflow-{}-forge.log", pair_id);
        // Source the env file (which contains secrets) before running, so
        // secrets never appear in the command string that is logged or in the
        // remote process argv.
        let remote_cmd = format!(
            "cd {} && . {} && nohup sh -c {} < {} > {} 2>&1 & echo $!",
            shell_quote(worktree),
            shell_quote(&env_file),
            shell_quote(&shell_cmd),
            shell_quote(&prompt_file),
            shell_quote(&log_path),
        );
        // Log the command being sent (shell_cmd contains only program+args,
        // no secrets — those are in the env file which is never logged).
        info!(
            pair = pair_id,
            ticket = ticket_id,
            shell_cmd = %shell_cmd,
            env_file = %env_file,
            prompt_file = %prompt_file,
            log_path = %log_path,
            "FORGE remote command constructed"
        );
        let output = client
            .workspace_exec_with_timeout(&workspace_id, &remote_cmd, 120)
            .await
            .context("Failed to spawn FORGE in Coder workspace")?;

        if output.exit_code != 0 {
            anyhow::bail!(
                "FORGE spawn command failed in Coder workspace (exit {}): {}",
                output.exit_code,
                output.stderr
            );
        }

        let pid = output.stdout.trim();
        if pid.is_empty() {
            anyhow::bail!("FORGE spawn command did not return a PID");
        }
        // The PID comes from untrusted remote stdout — reject anything that is
        // not a clean integer before embedding it in `coder-pid-<pid>` (which
        // is later interpolated into remote `kill` commands).
        if !pid.chars().all(|c| c.is_ascii_digit()) {
            anyhow::bail!(
                "FORGE spawn command returned a non-numeric PID (got {:?}) — refusing to use it",
                pid
            );
        }

        info!(
            pair = pair_id,
            ticket = ticket_id,
            workspace_id = %workspace_id,
            pid,
            "FORGE spawned in Coder workspace"
        );

        // Brief delay then check if the process survived startup.  If it died
        // immediately, read the log file so the error is visible in the
        // blocked reason instead of a generic "exited N times" message.
        tokio::time::sleep(Duration::from_millis(500)).await;
        let alive = {
            let check = format!("kill -0 {}", pid);
            client
                .workspace_exec_with_timeout(&workspace_id, &check, 10)
                .await
                .map(|o| o.exit_code == 0)
                .unwrap_or(false)
        };
        if !alive {
            let log_content = client
                .workspace_read_file(&workspace_id, &log_path)
                .await
                .unwrap_or_default();
            error!(
                pair = pair_id,
                pid,
                log = %tail_truncate(&log_content, 1200),
                "FORGE process died immediately after spawn — see log above (tail of log shown; the actual error normally follows the SessionStart hook banner)"
            );
        }

        Ok(ManagedProcess::Coder(crate::coder_process::CoderTaskHandle {
            task_id: format!("coder-pid-{}", pid),
            workspace_id,
            client: Arc::new(client),
            spawn_time: std::time::Instant::now(),
        }))
    }

    #[cfg(feature = "coder")]
    async fn spawn_coder_sentinel(
        &self,
        pair_id: &str,
        ticket_id: &str,
        mode: SentinelMode,
        worktree: &Path,
        shared: &Path,
        timeout_secs: u64,
        backend: CliBackend,
    ) -> Result<ManagedProcess> {
        let (client, workspace_id) = self.coder_context()?;
        let prompt = self.build_sentinel_prompt(shared, &mode);
        let prompt_file = format!("/tmp/agentflow-{}-sentinel.prompt", pair_id);
        client
            .workspace_write_file(&workspace_id, &prompt_file, &prompt)
            .await
            .context("Failed to write SENTINEL prompt into Coder workspace")?;

        let segment = mode.segment_value();
        let mut cmd = self.build_sentinel_command(backend, worktree, shared);
        self.configure_coder_command(
            &mut cmd,
            pair_id,
            ticket_id,
            &segment,
            worktree,
            shared,
            backend,
            &[("SPRINTLESS_SENTINEL_TIMEOUT_SECS", &timeout_secs.to_string())],
        );

        let (env_file, shell_cmd) =
            prepare_coder_remote_command(&client, &workspace_id, pair_id, &cmd).await?;
        let log_path = format!("/tmp/agentflow-{}-sentinel-{}.log", pair_id, segment);
        let remote_cmd = format!(
            "cd {} && . {} && sh -c {} < {} > {} 2>&1",
            shell_quote(shared),
            shell_quote(&env_file),
            shell_quote(&shell_cmd),
            shell_quote(&prompt_file),
            shell_quote(&log_path),
        );
        let task_client = client.clone();
        let task_workspace_id = workspace_id.clone();
        // Use a grace ceiling beyond the event-loop timeout so that the
        // event-loop timeout (which records the failure as a *timeout* via
        // `append_sentinel_failure`) always fires first and owns termination.
        // If the exec ever hits this ceiling it means the event loop itself
        // stalled; `kill_on_drop` on the `coder` process ensures the remote
        // shell is terminated when the handle is aborted.
        let exec_ceiling = timeout_secs.saturating_add(120);
        let handle = tokio::spawn(async move {
            let output = task_client
                .workspace_exec_with_timeout(&task_workspace_id, &remote_cmd, exec_ceiling)
                .await?;
            Ok(output.exit_code)
        });

        info!(
            pair = pair_id,
            ticket = ticket_id,
            mode = ?mode,
            workspace_id = %workspace_id,
            "SENTINEL spawned in Coder workspace"
        );

        Ok(ManagedProcess::CoderJoin(Some(handle)))
    }

    /// Apply Codex-specific settings (marketplace.json).
    fn apply_codex_extras(&self) -> Result<()> {
        let home = std::env::var("HOME")
            .or_else(|_| std::env::var("USERPROFILE"))
            .unwrap_or_else(|_| "/tmp".to_string());

        let agents_dir = PathBuf::from(&home).join(".agents").join("plugins");
        if !agents_dir.exists() {
            std::fs::create_dir_all(&agents_dir)
                .context("Failed to create .agents/plugins directory")?;
        }

        let marketplace_file = agents_dir.join("marketplace.json");
        let codex_plugin_source = PathBuf::from("orchestration/plugin");

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

        std::fs::write(
            &marketplace_file,
            serde_json::to_string_pretty(&marketplace)?,
        )
        .context("Failed to write marketplace.json")?;

        Ok(())
    }

    /// Spawn a FORGE process (long-running) with specified CLI backend.
    pub async fn spawn_forge_with_backend(
        &self,
        pair_id: &str,
        ticket_id: &str,
        worktree: &Path,
        shared: &Path,
        backend: CliBackend,
    ) -> Result<ManagedProcess> {
        info!(
            pair = pair_id,
            ticket = ticket_id,
            worktree = %worktree.display(),
            backend = ?backend,
            "Spawning FORGE process"
        );

        #[cfg(feature = "coder")]
        if matches!(
            self.workspace_provider,
            crate::types::WorkspaceProvider::Coder
        ) {
            return self
                .spawn_coder_forge(pair_id, ticket_id, worktree, shared, backend, false)
                .await;
        }

        // In Local mode, the CLI binary must exist on the host. Fail fast
        // with a clear error instead of waiting for OS error 2 at spawn time.
        // In Coder mode the binary runs inside the workspace — skip validation.
        if matches!(
            self.workspace_provider,
            crate::types::WorkspaceProvider::Local
        ) {
            let config = self.get_backend(backend);
            Self::validate_cli_binary(
                &config.binary_path,
                backend.binary_name(),
                &self.workspace_provider,
            )
            .map_err(|e| anyhow!("{}", e))?;
        }

        // Build the initial prompt for FORGE
        let initial_prompt = self.build_forge_prompt(shared);

        let mut cmd = self.build_cli_command(backend, worktree, shared);

        // Apply Codex marketplace plugin registration if needed
        let config = self.get_backend(backend);
        if config.needs_extras_provisioning {
            self.apply_codex_extras()?;
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

        // Set backend-specific home directory (CODEX_HOME) for isolated config
        Self::ensure_home_dir(&mut cmd, config, worktree);

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
        Ok(ManagedProcess::Local(child))
    }

    /// Spawn a FORGE process (long-running) using default backend.
    pub async fn spawn_forge(
        &self,
        pair_id: &str,
        ticket_id: &str,
        worktree: &Path,
        shared: &Path,
    ) -> Result<ManagedProcess> {
        self.spawn_forge_with_backend(pair_id, ticket_id, worktree, shared, self.default_backend)
            .await
    }

    pub async fn spawn_forge_resume(
        &self,
        pair_id: &str,
        ticket_id: &str,
        worktree: &Path,
        shared: &Path,
    ) -> Result<ManagedProcess> {
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
    ) -> Result<ManagedProcess> {
        info!(
            pair = pair_id,
            ticket = ticket_id,
            "Spawning FORGE process (PR creation mode)"
        );

        let backend = self.default_backend;

        #[cfg(feature = "coder")]
        if matches!(
            self.workspace_provider,
            crate::types::WorkspaceProvider::Coder
        ) {
            return self
                .spawn_coder_forge(pair_id, ticket_id, worktree, shared, backend, true)
                .await;
        }

        let initial_prompt = self.build_forge_pr_prompt(shared);

        let mut cmd = self.build_cli_command(backend, worktree, shared);

        // Apply Codex marketplace plugin registration if needed
        let config = self.get_backend(backend);
        if config.needs_extras_provisioning {
            self.apply_codex_extras()?;
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

        // Set backend-specific home directory (CODEX_HOME) for isolated config
        Self::ensure_home_dir(&mut cmd, config, worktree);

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
        Ok(ManagedProcess::Local(child))
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
    ) -> Result<ManagedProcess> {
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
    ) -> Result<ManagedProcess> {
        self.spawn_sentinel_with_backend(
            pair_id,
            ticket_id,
            mode,
            worktree,
            shared,
            timeout_secs,
            self.default_backend,
        )
        .await
    }

    /// Spawn a SENTINEL process with an explicit backend.
    #[allow(clippy::too_many_arguments)]
    pub async fn spawn_sentinel_with_backend(
        &self,
        pair_id: &str,
        ticket_id: &str,
        mode: SentinelMode,
        worktree: &Path,
        shared: &Path,
        timeout_secs: u64,
        backend: CliBackend,
    ) -> Result<ManagedProcess> {
        let segment = mode.segment_value();

        info!(
            pair = pair_id,
            ticket = ticket_id,
            mode = ?mode,
            segment = %segment,
            backend = ?backend,
            "Spawning SENTINEL process (ephemeral)"
        );

        #[cfg(feature = "coder")]
        if matches!(
            self.workspace_provider,
            crate::types::WorkspaceProvider::Coder
        ) {
            return self
                .spawn_coder_sentinel(pair_id, ticket_id, mode, worktree, shared, timeout_secs, backend)
                .await;
        }

        // Build the initial prompt for SENTINEL based on mode
        let initial_prompt = self.build_sentinel_prompt(shared, &mode);

        let mut cmd = self.build_sentinel_command(backend, worktree, shared);

        // Apply Codex marketplace plugin registration if needed
        let config = self.get_backend(backend);
        if config.needs_extras_provisioning {
            self.apply_codex_extras()?;
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

        // Set backend-specific home directory for isolated config
        Self::ensure_home_dir(&mut cmd, config, shared);

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
        Ok(ManagedProcess::Local(child))
    }

    /// Wait for a process to complete with timeout.
    pub async fn wait_with_timeout(
        &self,
        child: &mut ManagedProcess,
        timeout: Duration,
    ) -> Result<ProcessOutcome> {
        match child {
            ManagedProcess::Local(c) => {
                match tokio::time::timeout(timeout, c.wait()).await {
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
                        c.kill().await.context("Failed to kill timed-out process")?;
                        Ok(ProcessOutcome::Timeout)
                    }
                }
            }
            #[cfg(feature = "coder")]
            ManagedProcess::CoderJoin(opt) => {
                let Some(handle) = opt.as_mut() else {
                    return Ok(ProcessOutcome::Failed { exit_code: None });
                };
                match tokio::time::timeout(timeout, handle).await {
                    Ok(Ok(Ok(code))) => {
                        if code == 0 {
                            Ok(ProcessOutcome::Success)
                        } else {
                            warn!(exit_code = code, "Coder task exited with error");
                            Ok(ProcessOutcome::Failed { exit_code: Some(code) })
                        }
                    }
                    Ok(Ok(Err(e))) => {
                        error!(error = %e, "Coder task returned an error");
                        Ok(ProcessOutcome::Failed { exit_code: None })
                    }
                    Ok(Err(e)) => {
                        error!(error = %e, "Coder task join failed");
                        Ok(ProcessOutcome::Failed { exit_code: None })
                    }
                    Err(_) => {
                        warn!("Coder task timed out, aborting");
                        if let Some(h) = opt.take() {
                            h.abort();
                        }
                        Ok(ProcessOutcome::Timeout)
                    }
                }
            }
            #[cfg(feature = "coder")]
            ManagedProcess::Coder(_) => {
                // Long-running remote FORGE PID; not meant to be awaited here.
                Ok(ProcessOutcome::Failed { exit_code: None })
            }
        }
    }

    /// Kill a process.
    pub async fn kill(&self, child: &mut ManagedProcess) -> Result<()> {
        match child {
            ManagedProcess::Local(c) => {
                info!(pid = ?c.id(), "Killing process");
                c.kill().await.context("Failed to kill process")?;
            }
            #[cfg(feature = "coder")]
            ManagedProcess::Coder(handle) => {
                handle.kill().await?;
            }
            #[cfg(feature = "coder")]
            ManagedProcess::CoderJoin(opt) => {
                if let Some(handle) = opt.take() {
                    handle.abort();
                }
            }
        }
        Ok(())
    }

    /// Check if a process is still running.
    pub async fn is_running(&self, child: &mut ManagedProcess) -> bool {
        match child {
            ManagedProcess::Local(c) => matches!(c.try_wait(), Ok(None)),
            #[cfg(feature = "coder")]
            ManagedProcess::Coder(handle) => handle.is_running().await,
            #[cfg(feature = "coder")]
            ManagedProcess::CoderJoin(opt) => {
                opt.as_ref().map(|h| !h.is_finished()).unwrap_or(false)
            }
        }
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
            - SHARED DIRECTORY ({shared_path}): Write WORKLOG.md, STATUS.json, and COMMIT_MSG.md here\n\n\
            VALID STATUS.json VALUES — use only these exact strings in the \"status\" field:\n\
            - \"PR_OPENED\" — work complete, PR created (include pr_url, pr_number, branch)\n\
            - \"COMPLETE\" — all work done; write this if you cannot push (harness will push for you)\n\
            - \"BLOCKED\" — cannot proceed (include reason, blockers)\n\
            - \"FUEL_EXHAUSTED\" — budget/tokens exhausted\n\
            - \"PENDING_REVIEW\" — work paused, waiting for review\n\
            Do NOT use any other status value — it will be treated as BLOCKED and your work wasted.\n\n\
            COMMIT_MSG.md:\n\
            Write a COMMIT_MSG.md file in the shared directory describing what you changed and why.\n\
            First line = short subject, blank line, then body. The harness will use this as the git commit message\n\
            if it has to push for you.\n\n\
            CRITICAL: Follow the instructions in TASK.md exactly. This is a {mode} cycle — \
            do NOT re-implement already-completed segments. Focus ONLY on fixing the issues \
            described in the {mode} details above.\n\n\
            You MUST update {shared_path}/WORKLOG.md as you work — the watchdog will kill your \
            process if WORKLOG.md is not updated within 20 minutes.\n\n\
            After fixing issues:\n\
            - First try to commit and push yourself: `git add -A && git commit -F {shared_path}/COMMIT_MSG.md && git push`\n\
            - If push succeeds, write STATUS.json with status PR_OPENED\n\
            - If push fails, write STATUS.json with status COMPLETE — the harness will commit and push for you\n\
            - Do NOT write BLOCKED just because push failed — that wastes your completed work\n\n\
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
    pub async fn spawn(self) -> Result<ManagedProcess> {
        let manager = match (&self.redis_url, &self.proxy_url) {
            (Some(redis_url), Some(proxy_url)) => ProcessManager::with_proxy(
                self.github_token,
                Some(redis_url.clone()),
                proxy_url,
                &self.worktree,
                &self.shared,
            ),
            (Some(redis_url), None) => ProcessManager::with_redis(
                self.github_token,
                redis_url,
                &self.worktree,
                &self.shared,
            ),
            (None, Some(proxy_url)) => ProcessManager::with_proxy(
                self.github_token,
                None,
                proxy_url,
                &self.worktree,
                &self.shared,
            ),
            (None, None) => ProcessManager::new(self.github_token, &self.worktree, &self.shared),
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
    fn test_strip_provider_prefix() {
        assert_eq!(
            strip_provider_prefix("anthropic/claude-haiku-4-5-20251001"),
            "claude-haiku-4-5-20251001"
        );
        assert_eq!(strip_provider_prefix("openai/gpt-4o"), "gpt-4o");
        assert_eq!(
            strip_provider_prefix("fireworks/accounts/fireworks/models/llama-v3p1-8b-instruct"),
            "accounts/fireworks/models/llama-v3p1-8b-instruct"
        );
        assert_eq!(
            strip_provider_prefix("gemini/gemini-2.5-pro"),
            "gemini-2.5-pro"
        );
        assert_eq!(
            strip_provider_prefix("groq/llama-3.3-70b-versatile"),
            "llama-3.3-70b-versatile"
        );
        assert_eq!(
            strip_provider_prefix("claude-haiku-4-5-20251001"),
            "claude-haiku-4-5-20251001"
        );
        assert_eq!(strip_provider_prefix("gpt-4o"), "gpt-4o");
        assert_eq!(strip_provider_prefix(""), "");
    }

    #[test]
    fn test_sentinel_mode_segment_value() {
        assert_eq!(SentinelMode::PlanReview.segment_value(), "");
        assert_eq!(SentinelMode::SegmentEval(3).segment_value(), "3");
        assert_eq!(SentinelMode::FinalReview.segment_value(), "final");
    }

    #[test]
    fn test_plan_review_prompt_uses_shared_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        let manager = ProcessManager::new("ghp_test", &worktree, &shared);
        let prompt = manager.build_sentinel_prompt(&shared, &SentinelMode::PlanReview);

        assert!(prompt.contains("--- TICKET.md ---"));
        assert!(prompt.contains("Write ONLY to"));
        assert!(prompt.contains("CONTRACT.md"));
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
        let manager = ProcessManager::new(
            "ghp_test",
            Path::new("/tmp/worktree"),
            Path::new("/tmp/shared"),
        );
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
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        let manager = ProcessManager::new("ghp_test", &worktree, &shared);
        let prompt = manager.build_sentinel_prompt(&shared, &SentinelMode::SegmentEval(3));

        assert!(prompt.contains("SHARED:"));
        assert!(prompt.contains("--- CONTRACT.md ---"));
        assert!(prompt.contains("segment-3-eval.md"));
    }

    #[test]
    fn test_final_review_prompt_uses_shared_absolute_paths() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        let manager = ProcessManager::new("ghp_test", &worktree, &shared);
        let prompt = manager.build_sentinel_prompt(&shared, &SentinelMode::FinalReview);

        assert!(prompt.contains("SHARED:"));
        assert!(prompt.contains("--- CONTRACT.md ---"));
        assert!(prompt.contains("final-review.md"));
    }

    #[test]
    fn test_parse_codex_exec_output_from_turns_array() {
        let json = r#"[
            {
                "n": 0,
                "items": [
                    {"type": "message", "content": "Starting evaluation..."}
                ]
            },
            {
                "n": 1,
                "items": [
                    {"type": "tool_result", "output": "APPROVED - All tests passed"}
                ]
            }
        ]"#;

        let result = parse_codex_exec_output(json).unwrap();
        assert!(result.success);
        assert_eq!(result.turns.len(), 2);
        assert_eq!(
            result.result_text.as_deref(),
            Some("APPROVED - All tests passed")
        );
    }

    #[test]
    fn test_parse_codex_exec_output_from_full_result() {
        let json = r#"{
            "thread_id": "thread_abc123",
            "turns": [
                {
                    "n": 0,
                    "items": [
                        {"type": "message", "content": "Reviewing segment 1..."}
                    ]
                },
                {
                    "n": 1,
                    "items": [
                        {"type": "tool_result", "output": "NEEDS_WORK - Fix required in src/main.rs"}
                    ]
                }
            ]
        }"#;

        let result = parse_codex_exec_output(json).unwrap();
        assert!(result.success);
        assert_eq!(result.thread_id.as_deref(), Some("thread_abc123"));
        assert_eq!(result.turns.len(), 2);
        assert_eq!(
            result.result_text.as_deref(),
            Some("NEEDS_WORK - Fix required in src/main.rs")
        );
    }

    #[test]
    fn test_parse_codex_exec_output_raw_text() {
        let raw = "This is not valid JSON, should be treated as raw text";

        let result = parse_codex_exec_output(raw).unwrap();
        assert!(result.success);
        assert_eq!(result.result_text.as_deref(), Some(raw));
        assert!(result.turns.is_empty());
    }

    #[test]
    fn test_parse_codex_exec_output_empty() {
        let result = parse_codex_exec_output("").unwrap();
        assert!(!result.success);
        assert!(result.result_text.is_none());
        assert!(result.turns.is_empty());
    }

    #[test]
    fn test_codex_home_set_for_backend() {
        let dir = tempfile::tempdir().unwrap();
        let worktree = dir.path().join("worktree");
        let shared = dir.path().join("shared");
        std::fs::create_dir_all(&worktree).unwrap();
        std::fs::create_dir_all(&shared).unwrap();
        let manager = ProcessManager::new("ghp_test", &worktree, &shared);

        // Verify that CODEX_HOME would be set correctly for Codex backend
        let config = manager.get_backend(CliBackend::Codex);
        let expected_codex_home = worktree.join(".codex-home");
        assert_eq!(config.home_dir(&worktree), expected_codex_home);
    }
}
