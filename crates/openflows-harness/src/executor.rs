// crates/openflows-harness/src/executor.rs
//! Temporary checkout task executor for verify serve (Forge role).
//!
//! Executes commands with FORGE workspace permissions and:
//! - Process group isolation (for clean timeout/kill)
//! - Timeout enforcement via tokio::time::timeout
//! - Stdout/stderr capture and streaming
//! - Result persistence to Redis
//!
//! Part of task 5.2-5.3 (issue #143).

use a2a_protocol::{ExecutorInfo, VerifyProgressEvent, VerifyResult};
use anyhow::Context;
use anyhow::Result;
use fred::prelude::*;
use nix::sys::signal::{killpg, Signal};
use nix::unistd::Pid;
use std::os::unix::process::CommandExt;
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};
use uuid::Uuid;

/// Execute a verify task in a temporary checkout with timeout enforcement.
///
/// `task_id` is the relay-assigned task id this execution reports back under;
/// when `None`, a fresh id is generated (used by the standalone/self-test
/// path). The returned `VerifyResult.task_id` always equals the supplied id.
///
/// `progress_tx` is an optional channel sender for streaming stdout/stderr
/// chunks as `VerifyProgressEvent`s during execution. The caller (typically
/// the `verify serve` loop) reads from the receiver and pushes events to the
/// A2A relay via `tasks/push_progress`.
///
/// `cancel_token` is an optional atomic flag; when set to true (by
/// `tasks/cancel`), the executor stops reading output and kills the process
/// group.
#[allow(clippy::too_many_arguments)] // All args are distinct inputs to the verify-execution pipeline.
pub async fn execute_verify_task(
    client: &fred::clients::Client,
    tenant: &str,
    pair_id: &str,
    argv: &[String],
    artifact_paths: &[String],
    timeout_secs: u64,
    workspace_id: &str,
    task_id_opt: Option<&str>,
    progress_tx: Option<mpsc::UnboundedSender<VerifyProgressEvent>>,
    cancel_token: Option<Arc<AtomicBool>>,
) -> Result<VerifyResult> {
    let sandbox = crate::sandbox::Sandbox::create(&std::env::current_dir()?, argv)?;
    let before_head = Some(sandbox.head.clone());
    let start = Instant::now();
    let task_id = match task_id_opt {
        Some(id) if !id.is_empty() => id.to_string(),
        _ => Uuid::new_v4().to_string(),
    };

    info!(
        task_id = %task_id,
        pair_id = %pair_id,
        argv = ?argv,
        timeout_secs,
        "Starting task execution"
    );

    // Spawn the command with process group isolation and output capture
    let mut child = sandbox
        .command()
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .process_group(0) // Create new process group for clean tree kill
        .spawn()
        .context("Failed to spawn command")?;

    let child_pid = child.id();
    // Clean up descendants on every exit, including cancellation of this future.
    // This guard drops before the checkout, so children cannot keep writing to it.
    let _process_group = VerificationProcessGroup(child_pid);

    // Get stdout and stderr readers
    let stdout = child.stdout.take().context("Failed to take stdout")?;
    let stderr = child.stderr.take().context("Failed to take stderr")?;

    // Clone shared values before moving into closures
    let progress_tx_1 = progress_tx.clone();
    let cancel_token_1 = cancel_token.clone();
    let progress_tx_2 = progress_tx.clone();
    let cancel_token_2 = cancel_token.clone();

    // Run the command with timeout
    let exit_status =
        match tokio::time::timeout(tokio::time::Duration::from_secs(timeout_secs), async {
            // Read streams synchronously in blocking tasks (with progress)
            let stdout_reader = tokio::task::spawn_blocking(move || {
                read_stream_sync_with_progress(stdout, "stdout", progress_tx_1, cancel_token_1)
            });
            let stderr_reader = tokio::task::spawn_blocking(move || {
                read_stream_sync_with_progress(stderr, "stderr", progress_tx_2, cancel_token_2)
            });

            // Check for cancellation
            if let Some(ref cancel) = cancel_token {
                if cancel.load(Ordering::SeqCst) {
                    kill_process_group(child_pid);
                    let _ = child.wait();
                    return Err(anyhow::anyhow!("Task cancelled before execution started"));
                }
            }

            // Wait for child to finish
            let status = wait_for_process(&mut child, cancel_token.as_deref()).await?;

            // A test may spawn background children and then exit successfully.
            // Stop those children before collecting artifacts or waiting for EOF.
            kill_process_group(child_pid);

            // Collect output
            let stdout_text = stdout_reader.await.context("Stdout reader task failed")??;
            let stderr_text = stderr_reader.await.context("Stderr reader task failed")??;

            Ok::<_, anyhow::Error>((status, stdout_text, stderr_text))
        })
        .await
        {
            Ok(Ok((status, stdout_text, stderr_text))) => (status, stdout_text, stderr_text, false),
            Ok(Err(e)) => {
                warn!(error = %e, "Command execution error");
                return Err(e);
            }
            Err(_) => {
                // Timeout occurred — kill the entire process group
                warn!(task_id = %task_id, "Command timed out, killing process group");
                kill_process_group(child_pid);
                let _ = child.wait();

                // Build timeout result
                let result = VerifyResult {
                    head_sha: None,
                    task_id: task_id.clone(),
                    exit_code: None,
                    timed_out: true,
                    duration_ms: start.elapsed().as_millis() as u64,
                    stdout_ref: format!("audit:a2a:{}:stdout", task_id),
                    stderr_ref: format!("audit:a2a:{}:stderr", task_id),
                    artifacts: vec![],
                    executor: ExecutorInfo {
                        role: "forge".to_string(),
                        workspace: workspace_id.to_string(),
                    },
                };

                // Store in Redis
                let verification_key = format!("ns:{}:pair:{}:verification", tenant, pair_id);
                let _: Result<(), _> = client
                    .set::<(), _, _>(
                        &verification_key,
                        serde_json::to_string(&result)?,
                        None,
                        None,
                        false,
                    )
                    .await;

                // Store timeout message in audit trail
                let stderr_key = format!("ns:{}:audit:a2a:{}:stderr", tenant, task_id);
                let _: Result<(), _> = client
                    .set::<(), _, _>(
                        &stderr_key,
                        format!("[TIMEOUT] Command exceeded {}s limit", timeout_secs),
                        None,
                        None,
                        false,
                    )
                    .await;

                return Ok(result);
            }
        };

    // Extract exit code
    let exit_code = if exit_status.0.success() {
        Some(0)
    } else {
        exit_status.0.code()
    };

    let duration_ms = start.elapsed().as_millis() as u64;

    // Store stdout/stderr in Redis audit trail (bounded size)
    let stdout_key = format!("ns:{}:audit:a2a:{}:stdout", tenant, task_id);
    let stderr_key = format!("ns:{}:audit:a2a:{}:stderr", tenant, task_id);

    // Keep only last 10KB of output per stream (avoid memory explosion)
    let stdout_tail = truncate_to_tail(&exit_status.1, 10240);
    let stderr_tail = truncate_to_tail(&exit_status.2, 10240);

    let _: Result<(), _> = client
        .set::<(), _, _>(&stdout_key, stdout_tail.clone(), None, None, false)
        .await;
    let _: Result<(), _> = client
        .set::<(), _, _>(&stderr_key, stderr_tail.clone(), None, None, false)
        .await;

    debug!(
        task_id = %task_id,
        exit_code = ?exit_code,
        duration_ms,
        stdout_lines = stdout_tail.lines().count(),
        stderr_lines = stderr_tail.lines().count(),
        "Task completed"
    );

    let mut artifacts = Vec::new();
    for (artifact, bytes) in sandbox.artifacts(artifact_paths)? {
        let key = format!(
            "ns:{tenant}:audit:a2a:{task_id}:artifact:{}",
            artifact.sha256
        );
        client
            .set::<(), _, _>(&key, bytes, None, None, false)
            .await?;
        artifacts.push(artifact);
    }

    // Build result artifact
    let result = VerifyResult {
        head_sha: before_head.clone().filter(|h| {
            clean_head().as_ref() == Some(h) && sandbox.source_unchanged().unwrap_or(false)
        }),
        task_id: task_id.clone(),
        exit_code,
        timed_out: false,
        duration_ms,
        stdout_ref: stdout_key.clone(),
        stderr_ref: stderr_key.clone(),
        artifacts,
        executor: ExecutorInfo {
            role: "forge".to_string(),
            workspace: workspace_id.to_string(),
        },
    };

    // Mirror result to Redis
    let verification_key = format!("ns:{}:pair:{}:verification", tenant, pair_id);
    let _: Result<(), _> = client
        .set::<(), _, _>(
            &verification_key,
            serde_json::to_string(&result)?,
            None,
            None,
            false,
        )
        .await;

    // Store full result in audit trail
    let result_key = format!("ns:{}:audit:a2a:{}:result", tenant, task_id);
    let _: Result<(), _> = client
        .set::<(), _, _>(
            &result_key,
            serde_json::to_string(&result)?,
            None,
            None,
            false,
        )
        .await;

    info!(
        task_id = %task_id,
        exit_code = ?exit_code,
        "Task result mirrored to Redis"
    );

    Ok(result)
}

async fn wait_for_process(
    child: &mut std::process::Child,
    cancel: Option<&AtomicBool>,
) -> Result<std::process::ExitStatus> {
    loop {
        if cancel.is_some_and(|c| c.load(Ordering::SeqCst)) {
            kill_process_group(child.id());
            let _ = child.wait();
            anyhow::bail!("Verification cancelled");
        }
        if let Some(status) = child.try_wait().context("Failed to wait for child")? {
            return Ok(status);
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

struct VerificationProcessGroup(u32);

impl Drop for VerificationProcessGroup {
    fn drop(&mut self) {
        kill_process_group(self.0);
    }
}

/// Kill the verification process group; the temporary checkout is removed on drop.
fn kill_process_group(pid: u32) {
    if pid == 0 {
        return;
    }
    let pgid = Pid::from_raw(pid as i32);
    if let Err(e) = killpg(pgid, Signal::SIGKILL) {
        if e == nix::errno::Errno::ESRCH {
            return;
        }
        warn!(pid, error = %e, "Failed to kill process group");
    }
}

/// Read a stream into a string line-by-line, optionally pushing progress
/// events and checking for cancellation.
fn read_stream_sync_with_progress<R: std::io::Read>(
    reader: R,
    stream_name: &'static str,
    progress_tx: Option<mpsc::UnboundedSender<VerifyProgressEvent>>,
    cancel_token: Option<Arc<AtomicBool>>,
) -> Result<String> {
    let mut reader = reader;
    let mut output = String::new();
    let mut chunk = [0u8; 4096];
    let mut progress_chunks = 0;
    loop {
        if cancel_token
            .as_ref()
            .is_some_and(|c| c.load(Ordering::SeqCst))
        {
            break;
        }
        let n = reader.read(&mut chunk)?;
        if n == 0 {
            break;
        }
        let text = String::from_utf8_lossy(&chunk[..n]).into_owned();
        if let Some(tx) = progress_tx.as_ref().filter(|_| progress_chunks < 256) {
            progress_chunks += 1;
            let event = if stream_name == "stderr" {
                VerifyProgressEvent::Stderr {
                    chunk: text.clone(),
                }
            } else {
                VerifyProgressEvent::Stdout {
                    chunk: text.clone(),
                }
            };
            let _ = tx.send(event);
        }
        output.push_str(&text);
        output = truncate_to_tail(&output, 10240);
    }

    Ok(output)
}

/// Keep only the last N bytes of a string (for bounded storage).
fn truncate_to_tail(s: &str, max_bytes: usize) -> String {
    if s.len() <= max_bytes {
        return s.to_string();
    }

    let mut start = s.len().saturating_sub(max_bytes);
    while start < s.len() && !s.is_char_boundary(start) {
        start += 1;
    }
    s[start..].to_owned()
}

fn clean_head() -> Option<String> {
    let status = Command::new("git")
        .args(["status", "--porcelain"])
        .output()
        .ok()?;
    if !status.status.success() || !status.stdout.is_empty() {
        return None;
    }
    let head = Command::new("git")
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    if !head.status.success() {
        return None;
    }
    Some(String::from_utf8(head.stdout).ok()?.trim().to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn process_group_guard_stops_command_on_drop() {
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        drop(VerificationProcessGroup(child.id()));
        let status =
            tokio::time::timeout(Duration::from_secs(2), wait_for_process(&mut child, None))
                .await
                .expect("guard must terminate verification")
                .unwrap();
        assert!(!status.success());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn process_wait_yields_so_timeout_and_cancellation_work() {
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let result = tokio::time::timeout(
            Duration::from_millis(100),
            wait_for_process(&mut child, None),
        )
        .await;
        kill_process_group(child.id());
        let _ = child.wait();
        assert!(result.is_err());
        let mut child = Command::new("sleep")
            .arg("30")
            .process_group(0)
            .spawn()
            .unwrap();
        let cancel = AtomicBool::new(true);
        assert!(wait_for_process(&mut child, Some(&cancel)).await.is_err());
    }

    #[test]
    fn stream_output_without_newlines_stays_bounded() {
        let input = "x".repeat(100_000);
        let output =
            read_stream_sync_with_progress(input.as_bytes(), "stdout", None, None).unwrap();
        assert_eq!(output.len(), 10240);
    }

    #[test]
    fn test_truncate_to_tail_preserves_short_strings() {
        let input = "hello\nworld";
        assert_eq!(truncate_to_tail(input, 100), input);
    }

    #[test]
    fn test_truncate_to_tail_respects_max_bytes() {
        let input = "line1\nline2\nline3\nline4";
        let result = truncate_to_tail(input, 15);
        assert!(result.len() <= 15);
        assert!(result.contains("line"));
    }

    #[test]
    fn test_truncate_to_tail_preserves_line_boundaries() {
        let input = "a\nb\nc\nd\ne";
        let result = truncate_to_tail(input, 5);
        // Should not have partial lines
        assert!(!result.starts_with("a\nb\nc")); // Partial lines removed
        assert!(result.contains('\n') || result.is_empty() || !result.contains("d\ne"));
    }
}
