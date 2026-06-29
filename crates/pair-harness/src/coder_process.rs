// crates/pair-harness/src/coder_process.rs
//! Coder-aware process management.
//!
//! When `workspace_provider == Coder`, FORGE and SENTINEL agents are spawned
//! inside the Coder workspace via the exec API instead of as local processes.
//!
//! Since Coder's exec API is synchronous (you submit a command and it runs to
//! completion), FORGE is run with `nohup ... &` to background it, and progress
//! is monitored via workspace file changes (the existing SharedStore-based
//! event detection system handles this).

use anyhow::{bail, Context, Result};
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;
use tracing::{error, info, warn};

#[cfg(feature = "coder")]
use crate::process::BackendConfig;
use crate::process::SentinelMode;
use crate::types::CliBackend;

/// Handle to a long-running task inside a Coder workspace.
#[cfg(feature = "coder")]
pub struct CoderTaskHandle {
    pub task_id: String,
    pub workspace_id: String,
    pub client: Arc<coder_client::CoderClient>,
    pub spawn_time: Instant,
}

#[cfg(feature = "coder")]
impl CoderTaskHandle {
    /// Check if the task is still running by examining the process list.
    pub async fn is_running(&self) -> bool {
        let check_cmd = format!("ps aux | grep -v grep | grep {}", self.task_id);
        match self.client.workspace_exec_with_timeout(&self.workspace_id, &check_cmd, 10).await {
            Ok(output) => output.exit_code == 0 && !output.stdout.trim().is_empty(),
            Err(_) => false,
        }
    }

    /// Kill the task by its PID (embedded in task_id).
    pub async fn kill(&self) -> Result<()> {
        let parts: Vec<&str> = self.task_id.rsplitn(2, '-').collect();
        if parts.len() < 2 {
            error!(task_id = %self.task_id, "Cannot parse PID from task ID");
            return Ok(());
        }
        let pid = parts[0];
        let _ = self.client
            .workspace_exec_with_timeout(&self.workspace_id, &format!("kill -9 {}", pid), 10)
            .await;
        info!(task_id = %self.task_id, pid, "Killed Coder workspace task");
        Ok(())
    }
}

/// Build the CLI command string for a given backend. This mirrors
/// `build_cli_command` from `process.rs` but produces a shell command
/// instead of a `tokio::process::Command`.
#[cfg(feature = "coder")]
fn build_cli_command_string(
    backend: CliBackend,
    config: &BackendConfig,
    model: Option<&str>,
    redis_url: Option<&str>,
    shared_path: Option<&Path>,
) -> String {
    let mut parts: Vec<String> = vec![config.binary_path.to_string_lossy().to_string()];

    for flag in &config.base_flags {
        parts.push(quote_arg(flag));
    }

    if let Some(m) = model {
        parts.push("--model".to_string());
        parts.push(quote_arg(m));
    }

    match backend {
        CliBackend::Codex => {
            // Add Codex-specific disable flags for non-function tool types.
            // These are the same flags appended by process.rs::append_sse_disable_flags.
            for flag in &["computer_use", "browser_use", "browser_use_external",
                         "image_generation", "tool_call_mcp_elicitation", "in_app_browser",
                         "tool_suggest", "apps", "multi_agent", "plugins", "plugin_hooks",
                         "plugin_sharing", "skill_mcp_dependency_install", "goals",
                         "guardian_approval", "workspace_dependencies"] {
                parts.push("--disable".to_string());
                parts.push(quote_arg(flag));
            }
        }
        _ => {}
    }

    if let Some(rd) = redis_url {
        parts.push(format!("SPRINTLESS_REDIS_URL={}", quote_arg(rd)));
    } else if let Some(sp) = shared_path {
        parts.push(format!("SPRINTLESS_STATE_FILE={}", quote_arg(&sp.to_string_lossy())));
    }

    // Set environment for model routing
    if let Some(m) = model {
        parts.push(format!("ANTHROPIC_MODEL={}", quote_arg(m)));
        parts.push(format!("OPENAI_MODEL={}", quote_arg(m)));
    }

    parts.join(" ")
}

#[cfg(feature = "coder")]
fn quote_arg(s: &str) -> String {
    if s.contains(' ') || s.contains('\'') || s.contains('"') || s.contains('$') {
        format!("'{}'", s.replace('\'', "'\\''"))
    } else {
        s.to_string()
    }
}

/// Submit a FORGE task to a Coder workspace.
/// The command is run via `workspace_exec` with a long timeout.
/// FORGE runs in the background inside the workspace, writing results to
/// workspace files that the SharedStore event detector picks up.
#[cfg(feature = "coder")]
pub async fn spawn_forge_coder(
    client: Arc<coder_client::CoderClient>,
    workspace_id: &str,
    backend: CliBackend,
    config: &BackendConfig,
    model_backend: Option<&str>,
    redis_url: Option<&str>,
    shared_path: &Path,
    pair_id: &str,
    ticket_id: &str,
    prompt_file: &str,
    worktree_path: &str,
) -> Result<CoderTaskHandle> {
    info!(
        pair_id, ticket_id, workspace_id, backend = ?backend,
        "Spawning FORGE via Coder workspace exec"
    );

    let cmd_string = build_cli_command_string(
        backend, config, model_backend, redis_url, Some(shared_path));

    // Run FORGE in a detached shell so it persists beyond the exec timeout.
    // The CLI reads the prompt from a file instead of stdin.
    let task_id = format!("forge-{}", pair_id);
    let spawn_cmd = format!(
        "cd {} && {} --prompt-file {} < /dev/null > /tmp/agentflow-{}.log 2>&1 & echo $!",
        worktree_path, cmd_string, prompt_file, pair_id
    );

    let output = client
        .workspace_exec_with_timeout(workspace_id, &spawn_cmd, 120)
        .await
        .with_context(|| format!("Failed to spawn FORGE in Coder workspace: {}", workspace_id))?;

    if output.exit_code != 0 {
        bail!(
            "FORGE spawn command failed (exit {}): {}",
            output.exit_code, output.stderr
        );
    }

    // Parse PID from stdout
    let pid = output.stdout.trim().parse::<u32>().unwrap_or(0);

    info!(
        pair_id, pid, task_id = %task_id, workspace_id,
        "FORGE spawned in Coder workspace"
    );

    Ok(CoderTaskHandle {
        task_id: format!("forge-task-{}", pid),
        workspace_id: workspace_id.to_string(),
        client,
        spawn_time: Instant::now(),
    })
}

/// Submit a SENTINEL task to a Coder workspace and wait for completion.
/// Since SENTINEL is ephemeral, we use `workspace_exec` synchronously.
#[cfg(feature = "coder")]
pub async fn spawn_sentinel_coder(
    client: &coder_client::CoderClient,
    workspace_id: &str,
    backend: CliBackend,
    config: &BackendConfig,
    model_backend: Option<&str>,
    redis_url: Option<&str>,
    shared_path: &Path,
    pair_id: &str,
    ticket_id: &str,
    mode: &SentinelMode,
    timeout_secs: u64,
    worktree_path: &str,
) -> Result<i32> {
    info!(
        pair_id, ticket_id, mode = ?mode, workspace_id, backend = ?backend,
        "Spawning SENTINEL via Coder workspace exec"
    );

    let cmd_string = build_cli_command_string(
        backend, config, model_backend, redis_url, Some(shared_path));

    let segment = mode.segment_value();

    // SENTINEL runs synchronously inside the Coder workspace.
    // It reads the prompt from a file and writes results to workspace files.
    let sentinel_cmd = format!(
        "cd {} && SPRINTLESS_PAIR_ID={} SPRINTLESS_TICKET_ID={} \
         SPRINTLESS_SEGMENT={} SPRINTLESS_WORKTREE={} \
         SPRINTLESS_SHARED={} {} --prompt-file /tmp/sentinel-prompt-{}.txt",
        shared_path.display(),
        pair_id,
        ticket_id,
        segment,
        worktree_path,
        shared_path.to_string_lossy(),
        cmd_string,
        pair_id,
    );

    let output = client
        .workspace_exec_with_timeout(workspace_id, &sentinel_cmd, timeout_secs)
        .await
        .with_context(|| format!("Failed to spawn SENTINEL in Coder workspace"))?;

    if output.exit_code != 0 {
        warn!(
            exit_code = output.exit_code,
            stderr = ?output.stderr,
            "SENTINEL exited with error in Coder workspace"
        );
    }

    Ok(output.exit_code)
}

/// Check if FORGE is still running in a Coder mode context.
/// Since we don't track the actual process, we check for indicators.
#[cfg(feature = "coder")]
pub fn is_forge_alive() -> bool {
    // In Coder mode, we rely on workspace file changes to detect progress.
    // The event loop timeout handles liveness checking.
    true
}

/// Coder-specific sentinel state tracking.
#[cfg(feature = "coder")]
pub struct CoderSentinelTracker {
    pub mode: SentinelMode,
    pub spawn_time: Instant,
    pub timeout_secs: u64,
}
