//! Harness SharedStore — typed, validated Redis I/O with tenant namespacing.
//!
//! All keys are prefixed with `ns:{tenant}:` for tenant isolation.
//! All writes are validated against serde schemas from `config::state`.

use a2a_protocol::{VerifyCwd, VerifyExpect, VerifyKind, VerifyProgressEvent, VerifyRequest};
use anyhow::{bail, Context, Result};
use config::lifecycle::{Event, Phase};
use config::state::{full_ticket_key, full_ticket_key_flat, heartbeat_key, HeartbeatRecord};
use fred::prelude::*;
use pocketflow_core::SharedStore;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Gate approval payload written by SENTINEL to allow FORGE to proceed.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GateApproval {
    pub phase: String,
    pub approved_by: String,
    pub ts: u64,
    pub notes: Option<String>,
}

/// Valid verdicts for the `review submit` command.
const VALID_VERDICTS: &[&str] = &["approve", "reject"];

/// Dispatch payload written by the Controller for a worker to read.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DispatchPayload {
    pub ticket_id: String,
    pub title: String,
    pub body: String,
    pub branch: Option<String>,
    pub contract_path: Option<String>,
}

/// PR info written by the harness when forge opens a PR.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PrInfo {
    pub pr_number: u64,
    pub branch: String,
    pub title: String,
}

/// Handoff payload written by forge for sentinel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct HandoffPayload {
    pub contract_md: String,
    pub notes: Option<String>,
}

/// Review payload written by sentinel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ReviewPayload {
    pub verdict: String,
    pub report: String,
    pub pr_number: Option<u64>,
    pub revision: u64,
    pub round: u64,
    pub head: String,
}

/// Merge payload written by vessel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergePayload {
    pub pr_number: u64,
    pub sha: String,
    pub merged: bool,
}

/// Exact artifact identity supplied by a reviewer, never inferred at submission time.
pub struct ReviewTarget {
    pub revision: u64,
    pub round: u64,
    pub head: Option<String>,
}

pub struct HarnessStore {
    client: fred::clients::Client,
    tenant: String,
    lifecycle: SharedStore,
}

/// Authorize an approver role for a gated phase transition. Only SENTINEL may
/// approve a gate; FORGE/other roles must be rejected so an agent cannot approve
/// its own plan and sidestep the mandatory review checkpoint. Comparison is
/// case-insensitive because the worker CLI derives the role from
/// `OPENFLOWS_ROLE` and operators may spell the role with varying case.
pub fn authorize_gate_approver(role: &str) -> Result<()> {
    if !role.eq_ignore_ascii_case("sentinel") {
        bail!(
            "Gate approval rejected: approver role '{}' is not SENTINEL. \
             Only SENTINEL may approve a gated phase transition; FORGE/other \
             roles are not authorized to approve their own plan.",
            role
        );
    }
    Ok(())
}

impl HarnessStore {
    pub async fn verification_repair(
        &self,
        ticket: &str,
        role: &str,
        reason: String,
    ) -> Result<()> {
        let state = self.lifecycle.lifecycle(ticket).await?;
        self.lifecycle
            .transition(
                ticket,
                state.version,
                role,
                Event::BlockVerification { reason },
            )
            .await?;
        Ok(())
    }

    pub async fn lifecycle_state(&self, ticket: &str) -> Result<config::lifecycle::Lifecycle> {
        self.lifecycle.lifecycle(ticket).await
    }

    pub async fn new(redis_url: &str, tenant: &str) -> Result<Self> {
        let config = Config::from_url(redis_url)?;
        let client = Builder::from_config(config).build()?;
        client.init().await.context("Failed to connect to Redis")?;
        Ok(Self {
            client,
            tenant: tenant.to_string(),
            lifecycle: SharedStore::new_redis_with_tenant(redis_url, Some(tenant.to_owned()))
                .await?,
        })
    }

    /// Build a tenant-namespaced key.
    fn key(&self, k: &str) -> String {
        format!("ns:{}:{}", self.tenant, k)
    }

    /// Read the dispatch payload for this ticket+role.
    pub async fn dispatch_read(&self, ticket: &str, role: &str) -> Result<()> {
        let key = self.key(&full_ticket_key(ticket, "dispatch", role));
        let val: Option<String> = self.client.get(&key).await.context("Redis GET failed")?;
        match val {
            Some(json_str) => {
                let payload: DispatchPayload =
                    serde_json::from_str(&json_str).context("Failed to parse dispatch payload")?;
                let output = serde_json::to_string_pretty(&payload)?;
                println!("{}", output);
                debug!(key = %key, "dispatch read");
            }
            None => {
                bail!(
                    "No dispatch found for ticket {} role {}. \
                     The Controller may not have assigned work yet.",
                    ticket,
                    role
                );
            }
        }
        Ok(())
    }

    /// Advance through the authoritative graph with one atomic lifecycle write.
    /// Testing captures a clean checkout head; later stages require its evidence.
    pub async fn status_set(&self, ticket: &str, role: &str, phase: &str) -> Result<()> {
        let phase = Phase::parse(phase)?;
        let state = self.lifecycle.lifecycle(ticket).await?;
        let head = if phase == Phase::Testing {
            let head = Self::checkout_head()?;
            let environment =
                crate::sandbox::PreparedEnvironment::load(&std::env::current_dir()?, &head)?;
            anyhow::ensure!(environment.lifecycle_version == state.version || state.phase == Phase::Testing,
                "Lifecycle changed since preparation; run verify prepare again before entering testing");
            Some(head)
        } else {
            None
        };
        self.lifecycle
            .transition(ticket, state.version, role, Event::Move { phase, head })
            .await?;
        Ok(())
    }

    fn checkout_head() -> Result<String> {
        let dirty = std::process::Command::new("git")
            .args(["status", "--porcelain"])
            .output()?;
        anyhow::ensure!(
            dirty.status.success() && dirty.stdout.is_empty(),
            "Commit all work before testing; checkout must be clean"
        );
        let head = std::process::Command::new("git")
            .args(["rev-parse", "HEAD"])
            .output()?;
        anyhow::ensure!(head.status.success(), "Cannot identify checkout HEAD");
        Ok(String::from_utf8(head.stdout)?.trim().to_string())
    }

    pub async fn gate_decide(
        &self,
        ticket: &str,
        role: &str,
        phase: &str,
        approved: bool,
        report: &str,
        target: ReviewTarget,
    ) -> Result<()> {
        let ReviewTarget {
            revision,
            round,
            head,
        } = target;
        let phase = Phase::parse(phase)?;
        let state = self.lifecycle.lifecycle(ticket).await?;
        self.lifecycle
            .transition(
                ticket,
                state.version,
                role,
                Event::Decide {
                    round,
                    phase,
                    approved,
                    report: report.into(),
                    revision,
                    head,
                },
            )
            .await?;
        Ok(())
    }

    /// Legacy API intentionally refuses unversioned approval. Use gate_decide.
    pub async fn gate_approve(
        &self,
        _ticket: &str,
        _role: &str,
        _phase: &str,
        _notes: Option<&str>,
    ) -> Result<()> {
        bail!("Approval requires an explicit plan revision and tested head. Use gate decide --revision <N> --round <R> --phase <plan_ready|testing|submit> --verdict approve --report <file>.")
    }

    pub async fn gate_status(&self, ticket: &str, _phase: &str) -> Result<()> {
        self.status_get(ticket).await
    }

    /// Read the current status JSON for this ticket. Prints `{}` when unset
    /// so hook scripts can always parse the output.
    pub async fn status_get(&self, ticket: &str) -> Result<()> {
        println!(
            "{}",
            serde_json::to_string(&self.lifecycle.lifecycle(ticket).await?)?
        );
        Ok(())
    }

    /// Read the recorded PR info for this ticket. Prints `{}` when unset.
    pub async fn pr_get(&self, ticket: &str) -> Result<()> {
        let key = self.key(&full_ticket_key_flat(ticket, "pr"));
        let val: Option<String> = self.client.get(&key).await.context("Redis GET failed")?;
        println!("{}", val.unwrap_or_else(|| "{}".to_string()));
        debug!(key = %key, "pr read");
        Ok(())
    }

    /// Write a handoff contract (forge → sentinel).
    pub async fn handoff_write(
        &self,
        ticket: &str,
        contract_path: &Path,
        notes: Option<&str>,
    ) -> Result<()> {
        let contract_md = std::fs::read_to_string(contract_path).context(format!(
            "Failed to read contract file: {}",
            contract_path.display()
        ))?;
        let payload = HandoffPayload {
            contract_md,
            notes: notes.map(|s| s.to_string()),
        };
        let key = self.key(&full_ticket_key_flat(ticket, "handoff"));
        let json = serde_json::to_string(&payload)?;
        self.client
            .set::<(), _, _>(&key, json, None, None, false)
            .await
            .context("Redis write failed")?;
        println!("Wrote: {}", key);
        info!(key = %key, "handoff written");
        Ok(())
    }

    /// Record that a PR was opened.
    pub async fn pr_opened(&self, ticket: &str, pr: &u64, branch: &str, title: &str) -> Result<()> {
        let state = self.lifecycle.lifecycle(ticket).await?;
        self.lifecycle
            .transition(ticket, state.version, "forge", Event::Pr { number: *pr })
            .await?;
        let payload = PrInfo {
            pr_number: *pr,
            branch: branch.to_string(),
            title: title.to_string(),
        };
        let key = self.key(&full_ticket_key_flat(ticket, "pr"));
        let json = serde_json::to_string(&payload)?;
        self.client
            .set::<(), _, _>(&key, json, None, None, false)
            .await
            .context("Redis write failed")?;
        println!("Wrote: {} (pr #{})", key, pr);
        info!(key = %key, pr, "pr opened");
        Ok(())
    }

    /// Submit a review verdict (sentinel).
    ///
    /// Only SENTINEL may write the PR-review verdict. This mirrors
    /// [`authorize_gate_approver`]: FORGE/other roles must not be able to submit
    /// an `approve` for their own ticket, which would otherwise be treated as
    /// SENTINEL's verdict and let the builder bypass the independent reviewer.
    pub async fn review_submit(
        &self,
        ticket: &str,
        role: &str,
        verdict: &str,
        report_path: &Path,
        pr: Option<u64>,
        target: ReviewTarget,
    ) -> Result<()> {
        let ReviewTarget {
            revision,
            round,
            head,
        } = target;
        let head = head.context("PR review requires a commit SHA")?;
        if !role.eq_ignore_ascii_case("sentinel") {
            bail!(
                "Review submit rejected: role '{}' is not SENTINEL. \
                 Only SENTINEL may record a PR review verdict; FORGE/other roles \
                 cannot submit an approval for their own ticket.",
                role
            );
        }
        if !VALID_VERDICTS.contains(&verdict) {
            bail!(
                "Invalid verdict '{}'. Valid verdicts: {}",
                verdict,
                VALID_VERDICTS.join(", ")
            );
        }
        let report = std::fs::read_to_string(report_path).context(format!(
            "Failed to read report file: {}",
            report_path.display()
        ))?;
        let state = self.lifecycle.lifecycle(ticket).await?;
        anyhow::ensure!(
            state.phase == Phase::Submit
                && state.review_round == round
                && state.revision == revision
                && state.head.as_ref() == Some(&head),
            "Stale PR review"
        );
        anyhow::ensure!(
            pr.is_none() || pr == state.pr_number,
            "Review PR does not match lifecycle candidate"
        );
        self.lifecycle
            .transition(
                ticket,
                state.version,
                role,
                Event::Decide {
                    phase: Phase::Submit,
                    approved: verdict == "approve",
                    report,
                    revision,
                    round,
                    head: Some(head),
                },
            )
            .await?;

        Ok(())
    }

    /// Record that a merge completed (vessel).
    pub async fn merge_done(&self, _ticket: &str, _pr: &u64, _sha: &str) -> Result<()> {
        bail!("Only the controller records confirmed GitHub merges")
    }

    /// Start daemonized heartbeat writing (every 30s).
    pub async fn heartbeat_start(&self, ticket: &str, role: &str) -> Result<()> {
        let key = self.key(&heartbeat_key(role, ticket));
        info!(key = %key, "Starting heartbeat writer (30s interval)");

        loop {
            let record = HeartbeatRecord {
                ts: SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_secs(),
                ws_id: std::env::var("CODER_WORKSPACE_ID").unwrap_or_default(),
                status: "running".to_string(),
            };
            let json = serde_json::to_string(&record)?;
            self.client
                .set::<(), _, _>(
                    &key,
                    &json,
                    Some(fred::types::Expiration::EX(120)),
                    None,
                    false,
                )
                .await
                .context("Redis write failed")?;
            debug!(key = %key, "heartbeat written");
            tokio::time::sleep(std::time::Duration::from_secs(30)).await;
        }
    }

    /// Stop heartbeat writing (delete the key).
    pub async fn heartbeat_stop(&self, ticket: &str, role: &str) -> Result<()> {
        let key = self.key(&heartbeat_key(role, ticket));
        let _: Result<i64, _> = self.client.del(&key).await;
        println!("Deleted: {}", key);
        info!(key = %key, "heartbeat stopped");
        Ok(())
    }

    /// Submit a verify request (SENTINEL-side, task 3 of issue #143).
    /// Sends A2A request to nexus relay, streams progress, writes final result to stdout as JSON.
    pub async fn verify_request(
        &self,
        ticket: &str,
        argv: Vec<String>,
        timeout_secs: u64,
        expect_exit: Option<i32>,
        artifacts: Option<&str>,
    ) -> Result<()> {
        a2a_protocol::validate_command_argv(&argv)?;
        let state = self.lifecycle.lifecycle(ticket).await?;
        anyhow::ensure!(
            state.phase == Phase::Testing && state.head.is_some(),
            "A2A verification requires testing"
        );
        anyhow::ensure!(
            expect_exit == Some(0),
            "Gate evidence requires --expect-exit 0"
        );
        // Create A2A client for this pair (ticket == pair_id in current design)
        let client = crate::a2a_client::A2AClient::new(ticket.to_string(), "sentinel".to_string())?;

        // Health check first
        client.health_check().await?;

        // Parse artifacts list if provided
        let artifact_list: Vec<String> = artifacts
            .map(|a| a.split(',').map(|s| s.trim().to_string()).collect())
            .unwrap_or_default();

        // Build VerifyRequest
        let request = VerifyRequest {
            pair_id: ticket.to_string(),
            kind: VerifyKind::Command,
            cwd: VerifyCwd::Repo, // Could be configurable
            argv,
            timeout_secs,
            env_allowlist: vec![], // Could be configurable
            expect: VerifyExpect {
                exit_code: expect_exit,
                artifacts: artifact_list,
            },
        };

        // Submit request to relay
        let task_id = client
            .submit_verify_request(&request)
            .await
            .context("Failed to submit verify request")?;

        info!(task_id = %task_id, pair_id = ticket, "Verify request submitted");

        // Poll the relay for the terminal result. The task passes through
        // pending → running (Forge) → completed, at which point the relay has
        // mirrored the result to Redis and `tasks/get` returns it. Bounded to
        // avoid an infinite loop if Forge never picks the task up.
        let deadline = std::time::Instant::now() + Duration::from_secs(request.timeout_secs + 60);
        loop {
            if let Some(result) = client.get_task_status(&task_id).await? {
                // The result is durable (mirrored by the relay before ack);
                // surface it as JSON for the caller.
                println!("{}", serde_json::to_string_pretty(&result)?);
                if result.timed_out {
                    bail!("verification timed out");
                }
                if result.exit_code.is_none() {
                    let diagnostic_key = self.key(&format!("audit:a2a:{task_id}:stderr"));
                    let diagnostic: Option<String> = self.client.get(&diagnostic_key).await?;
                    bail!(
                        "Verification executor failed: {}",
                        diagnostic
                            .as_deref()
                            .unwrap_or("inspect FORGE verify.log for runtime failure")
                    );
                }
                match request.expect.exit_code {
                    Some(expected) if result.exit_code != Some(expected) => {
                        bail!(
                            "verification failed: expected exit {}, got {:?}",
                            expected,
                            result.exit_code
                        )
                    }
                    _ => {}
                }
                anyhow::ensure!(
                    result.head_sha == state.head,
                    "Verification did not run on the clean candidate head"
                );
                self.lifecycle
                    .transition(
                        ticket,
                        state.version,
                        "sentinel",
                        Event::Verified {
                            head: state.head.clone().unwrap(),
                            task: task_id.clone(),
                        },
                    )
                    .await?;
                return Ok(());
            }

            if std::time::Instant::now() >= deadline {
                bail!(
                    "verification did not complete before deadline (task {})",
                    task_id
                );
            }

            tokio::time::sleep(Duration::from_millis(1000)).await;
        }
    }

    /// Long-running executor (FORGE-side, task 5 of issue #143).
    /// Subscribes to verify tasks from nexus relay, executes them in sandbox, returns results.
    /// This is the core executor implementation with full sandbox isolation.
    pub async fn verify_serve(&self, ticket: &str, role: &str) -> Result<()> {
        // Verify this is Forge role
        if !role.eq_ignore_ascii_case("forge") {
            bail!("verify serve requires FORGE role, got {}", role);
        }

        let client = crate::a2a_client::A2AClient::new(ticket.to_string(), role.to_string())?;

        // Health check
        client.health_check().await?;

        // Get workspace ID for audit trail
        let workspace_id =
            std::env::var("CODER_WORKSPACE_ID").unwrap_or_else(|_| "unknown".to_string());

        // Get tenant for Redis namespacing
        let env = config::EnvConfig::from_env()?;
        let tenant = env
            .tenant
            .tenant
            .clone()
            .context("OPENFLOWS_TENANT not set")?;

        println!(
            "✓ Forge verify executor ready (workspace: {}, ticket: {})",
            workspace_id, ticket
        );
        println!("  Listening for tasks from nexus A2A relay... (Ctrl+C to stop)");

        // Poll the relay for tasks assigned to this pair. Forge is the only
        // role that may claim (`tasks/claim` enforces this relay-side). Each
        // claimed task is executed in the sandbox and the terminal result is
        // submitted via `tasks/complete`, which mirrors it to Redis so
        // Sentinel's `tasks/get` can observe completion.
        loop {
            // Claim the next pending task for this pair.
            let claimed = match client.claim_next_task().await {
                Ok(c) => c,
                Err(e) => {
                    warn!(error = %e, "Failed to claim task; backing off");
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            let (task_id, request) = match claimed {
                Some(t) => t,
                None => {
                    // No work: brief pause before polling again.
                    tokio::time::sleep(Duration::from_secs(2)).await;
                    continue;
                }
            };

            info!(
                task_id = %task_id,
                pair_id = ticket,
                argv = ?request.argv,
                "Executing claimed verify task"
            );

            // Set up progress streaming channel
            let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<VerifyProgressEvent>();

            // Get a cancel token — starts local and is synced to the relay's
            // cancel state by a background polling task below.
            let cancel_token = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));

            // Spawn a background poller that checks the relay for cancellation
            // and sets the local token so the executor kills the child process.
            let cancel_token_for_poller = cancel_token.clone();
            let poller_task_id = task_id.clone();
            let poller_client =
                crate::a2a_client::A2AClient::new(ticket.to_string(), "forge".to_string());
            if let Ok(poller) = poller_client {
                tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(Duration::from_millis(500)).await;
                        match poller.get_task_status_str(&poller_task_id).await {
                            Ok(status) if status == "cancelled" => {
                                cancel_token_for_poller.store(true, Ordering::SeqCst);
                                break;
                            }
                            Ok(status) if status == "completed" => break,
                            Ok(_) => {}
                            Err(_) => break,
                        }
                    }
                });
            }

            // Spawn a task to forward progress events to the relay
            let progress_task_id = task_id.clone();
            let progress_client =
                crate::a2a_client::A2AClient::new(ticket.to_string(), "forge".to_string());
            let progress_handle = match progress_client {
                Ok(pc) => {
                    let client_for_progress = pc;
                    Some(tokio::spawn(async move {
                        while let Some(event) = progress_rx.recv().await {
                            if let Err(e) = client_for_progress
                                .push_progress(&progress_task_id, &event)
                                .await
                            {
                                debug!(error = %e, "Failed to push progress event (non-fatal)");
                            }
                        }
                    }))
                }
                Err(e) => {
                    warn!(error = %e, "Failed to create progress client; progress streaming disabled");
                    None
                }
            };

            let result = match crate::executor::execute_verify_task(
                &self.client,
                &tenant,
                &request.pair_id,
                &request.argv,
                &request.expect.artifacts,
                request.timeout_secs,
                &workspace_id,
                Some(&task_id),
                Some(progress_tx),
                Some(cancel_token.clone()),
            )
            .await
            {
                Ok(mut r) => {
                    // Ensure the result is attributed to the claimed task id
                    // (the executor falls back to a fresh id when absent).
                    r.task_id = task_id.clone();
                    r
                }
                Err(e) => {
                    // Execution failed before producing a result. Report a
                    // synthetic failure so the task does not hang pending
                    // forever on the Sentinel side.
                    eprintln!("  [TASK FAILED] {}: {}", task_id, e);
                    let stderr_key = self.key(&format!("audit:a2a:{}:stderr", task_id));
                    self.client
                        .set::<(), _, _>(
                            &stderr_key,
                            format!("[EXECUTOR_SETUP] {e:#}"),
                            None,
                            None,
                            false,
                        )
                        .await?;
                    let fail = a2a_protocol::VerifyResult {
                        head_sha: None,
                        task_id: task_id.clone(),
                        exit_code: None,
                        timed_out: false,
                        duration_ms: 0,
                        stdout_ref: format!("audit:a2a:{}:stdout", task_id),
                        stderr_ref: stderr_key,
                        artifacts: vec![],
                        executor: a2a_protocol::ExecutorInfo {
                            role: "forge".to_string(),
                            workspace: workspace_id.clone(),
                        },
                    };
                    fail
                }
            };

            // Drop the progress handle (completes when the stream ends)
            drop(progress_handle);

            if let Err(e) = client.complete_task(&result).await {
                warn!(error = %e, task_id = %task_id, "Failed to submit result; will not retry this task");
                eprintln!("  [ERROR] could not report result for {}: {}", task_id, e);
            } else {
                println!(
                    "  [DONE] task {} → exit_code={:?}, {}ms",
                    task_id, result.exit_code, result.duration_ms
                );
            }
        }
    }

    /// Write a plan artifact (FORGE → Redis at `pair:{id}:plan`).
    ///
    /// FORGE writes its plan at the current chat-specific path, then calls this
    /// to persist it directly to Redis SharedStore so SENTINEL (and NEXUS)
    /// can read it without relying on Coder API filesystem access.
    pub async fn plan_write(&self, ticket: &str, file_path: &Path) -> Result<()> {
        let content = std::fs::read_to_string(file_path)?;
        let state = self.lifecycle.lifecycle(ticket).await?;
        self.lifecycle
            .transition(
                ticket,
                state.version,
                "forge",
                Event::Plan {
                    content: content.clone(),
                },
            )
            .await?;
        // Compatibility projection; the lifecycle record is authoritative.
        self.client
            .set::<(), _, _>(
                self.key(&format!("pair:{ticket}:plan")),
                serde_json::to_string(&content)?,
                None,
                None,
                false,
            )
            .await?;
        Ok(())
    }

    /// Read a plan artifact from Redis (`pair:{id}:plan`) and print to stdout.
    ///
    /// SENTINEL uses this to retrieve the FORGE plan during planning gate review.
    /// Prints the plan content as raw markdown; prints nothing if unset.
    pub async fn plan_read(&self, ticket: &str) -> Result<()> {
        let state = self.lifecycle.lifecycle(ticket).await?;
        if !state.plan.is_empty() {
            println!("{}", state.plan);
            return Ok(());
        }
        let raw: Option<String> = self
            .client
            .get(self.key(&format!("pair:{ticket}:plan")))
            .await?;
        if let Some(raw) = raw {
            println!("{}", serde_json::from_str::<String>(&raw).unwrap_or(raw));
        }
        Ok(())
    }

    /// List recent verification results (humans/audit, task 3 of issue #143).
    pub async fn verify_list(&self, pair_id: Option<&str>) -> Result<()> {
        if let Some(id) = pair_id {
            // List results for a specific pair
            let key = self.key(&format!("pair:{}:verification", id));
            let json_result: Option<String> =
                self.client.get(&key).await.context("Redis GET failed")?;

            match json_result {
                Some(json) => {
                    // Parse and pretty-print
                    match serde_json::from_str::<serde_json::Value>(&json) {
                        Ok(result) => {
                            println!("Verification result for {}:", id);
                            println!("{}", serde_json::to_string_pretty(&result)?);
                        }
                        Err(e) => {
                            warn!(error = %e, "Failed to parse verification result");
                            println!("(unparsable result: {})", json);
                        }
                    }
                }
                None => {
                    println!("No verification results for {}", id);
                }
            }
        } else {
            // Enumerate all pair:*:verification keys (requires scan)
            // For now, just note that this requires Redis SCAN
            println!("✓ Verification results (enumeration requires Redis SCAN):");
            println!("  Use --pair-id <ID> to view specific results");
            println!("  Results stored under: pair:{{pair_id}}:verification");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_valid_verdicts() {
        assert!(VALID_VERDICTS.contains(&"approve"));
        assert!(VALID_VERDICTS.contains(&"reject"));
        assert!(!VALID_VERDICTS.contains(&"maybe"));
    }

    #[test]
    fn test_dispatch_payload_serde() {
        let payload = DispatchPayload {
            ticket_id: "T-42".to_string(),
            title: "Fix bug".to_string(),
            body: "The bug is in auth.rs".to_string(),
            branch: Some("forge-t-42".to_string()),
            contract_path: None,
        };
        let json = serde_json::to_string(&payload).unwrap();
        let decoded: DispatchPayload = serde_json::from_str(&json).unwrap();
        assert_eq!(decoded.ticket_id, "T-42");
        assert_eq!(decoded.title, "Fix bug");
    }

    #[test]
    fn test_key_namespacing() {
        let tenant = "acme";
        let ticket = "T-42";
        let key = format!(
            "ns:{}:{}",
            tenant,
            full_ticket_key(ticket, "dispatch", "forge")
        );
        assert_eq!(key, "ns:acme:ticket:T-42:dispatch:forge");
    }

    #[test]
    fn test_gate_approver_accepts_sentinel_case_insensitive() {
        assert!(authorize_gate_approver("sentinel").is_ok());
        assert!(authorize_gate_approver("SENTINEL").is_ok());
        assert!(authorize_gate_approver("Sentinel").is_ok());
    }

    #[test]
    fn test_gate_approver_rejects_forge_and_others() {
        // FORGE must never approve its own plan — the review checkpoint exists
        // to supervise it. Vessel/Lore/empty/unknown roles are also rejected.
        assert!(authorize_gate_approver("forge").is_err());
        assert!(authorize_gate_approver("vessel").is_err());
        assert!(authorize_gate_approver("lore").is_err());
        assert!(authorize_gate_approver("").is_err());
        assert!(authorize_gate_approver("admin").is_err());
    }
}
