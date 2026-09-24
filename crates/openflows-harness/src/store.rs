//! Harness SharedStore — typed, validated Redis I/O with tenant namespacing.
//!
//! All keys are prefixed with `ns:{tenant}:` for tenant isolation.
//! All writes are validated against serde schemas from `config::state`.

use a2a_protocol::{VerifyCwd, VerifyExpect, VerifyKind, VerifyProgressEvent, VerifyRequest};
use anyhow::{bail, Context, Result};
use config::state::{
    full_ticket_key, full_ticket_key_flat, heartbeat_key, review_verdict_key, HeartbeatRecord,
    REVIEW_TYPE_PR,
};
use fred::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// Valid phases for the `status set` command.
const VALID_PHASES: &[&str] = &["planning", "building", "testing", "review_ready", "blocked"];

/// Phases that require SENTINEL approval before transitioning FROM them.
/// FORGE cannot move past these phases until SENTINEL writes a gate approval.
const GATED_PHASES: &[&str] = &["planning"];

/// Phases a brand-new ticket (no recorded status yet) may enter directly.
/// FORGE must first sit in `planning` and earn a SENTINEL gate approval before
/// it is allowed into any downstream phase; `blocked` is permitted as a failure
/// escape hatch so a freshly-provisioned workspace can report an immediate
/// blocker without first pretending to plan. Anything else is rejected to
/// prevent FORGE from short-circuiting straight to `building`/`testing`/
/// `review_ready` and bypassing the planning-gate review entirely.
const ENTRY_PHASES: &[&str] = &["planning", "blocked"];

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
}

/// Merge payload written by vessel.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MergePayload {
    pub pr_number: u64,
    pub sha: String,
    pub merged: bool,
}

pub struct HarnessStore {
    client: fred::clients::Client,
    tenant: String,
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

/// True iff `phase` may be set as a ticket's FIRST recorded status (i.e. the
/// ticket has no prior status in Redis). FORGE must enter through `planning`
/// (or `blocked`, the failure escape hatch) so a fresh ticket cannot jump
/// straight to `building`/`testing`/`review_ready` and thereby skip the
/// SENTINEL-reviewed planning gate.
pub fn is_allowed_first_phase(phase: &str) -> bool {
    ENTRY_PHASES.contains(&phase)
}

/// If transitioning `current_phase -> target` requires crossing a SENTINEL
/// gate, return the gated phase being LEFT (the phase whose gate approval
/// must be consulted and consumed); otherwise return `None`.
///
/// A gate is crossed when the current phase is itself gated (e.g. `planning`)
/// and the target differs from it. Transitions among downstream phases (e.g.
/// `building -> testing`) do not require a fresh approval, and returning TO
/// a gated phase (`building -> planning`) is free — the next outbound
/// transition will be gated, which is where the approval is enforced.
pub fn gate_source_for_transition(current_phase: &str, target: &str) -> Option<&'static str> {
    if GATED_PHASES.contains(&current_phase) && current_phase != target {
        GATED_PHASES.iter().find(|&&p| p == current_phase).copied()
    } else {
        None
    }
}

impl HarnessStore {
    pub async fn new(redis_url: &str, tenant: &str) -> Result<Self> {
        let config = Config::from_url(redis_url)?;
        let client = Builder::from_config(config).build()?;
        client.init().await.context("Failed to connect to Redis")?;
        Ok(Self {
            client,
            tenant: tenant.to_string(),
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

    /// Set the current phase for this ticket.
    ///
    /// Enforces gated phase transitions: FORGE cannot move past `planning`
    /// until SENTINEL approves via `gate approve`, and the approval is
    /// consumed on the outbound transition so a later return to `planning`
    /// (e.g. for a revised plan) requires a fresh SENTINEL review. A brand-new
    /// ticket with no prior status must enter via `planning` (or `blocked`),
    /// so it cannot short-circuit straight to `building` and skip the gate.
    pub async fn status_set(&self, ticket: &str, role: &str, phase: &str) -> Result<()> {
        if !VALID_PHASES.contains(&phase) {
            bail!(
                "Invalid phase '{}'. Valid phases: {}",
                phase,
                VALID_PHASES.join(", ")
            );
        }

        // Read current phase (if any) and enforce gating.
        let status_key = self.key(&full_ticket_key_flat(ticket, "status"));
        let current_status: Option<String> = self
            .client
            .get(&status_key)
            .await
            .context("Redis GET failed")?;

        let current_phase: Option<String> = current_status
            .as_deref()
            .and_then(|s| serde_json::from_str::<serde_json::Value>(s).ok())
            .and_then(|v| v.get("phase").and_then(|p| p.as_str()).map(String::from));

        // Fresh ticket (no recorded status): must enter through the planning
        // gate. FORGE cannot bypass the SENTINEL-reviewed plan by writing
        // `building`/`testing`/`review_ready` as its first status.
        if current_phase.is_none() && !is_allowed_first_phase(phase) {
            bail!(
                "Cannot set first status to '{}' for ticket {}. FORGE must enter through \
                 'planning' and obtain SENTINEL gate approval before any downstream phase. \
                 (Set status to 'planning', or 'blocked' to report an immediate blocker.)",
                phase,
                ticket
            );
        }

        // For transitions that leave a gated phase, require a SENTINEL approval
        // and consume it atomically so the approval is single-use per planning
        // cycle even under concurrent transitions.
        if let Some(source_phase) = current_phase
            .as_deref()
            .and_then(|cur| gate_source_for_transition(cur, phase))
        {
            let gate_key = self.key(&format!("ticket:{}:gate:{}", ticket, source_phase));

            // Atomically GET-and-DELETE the approval in a single Redis
            // command. Using separate GET then DEL would let two concurrent
            // `status set building` calls both observe the approval before
            // either delete lands, so a single SENTINEL approval would
            // authorize multiple transitions. GETDEL returns the value it
            // removed (None if the key was already gone), and the key is gone
            // by the time the command returns — the loser of the race gets
            // None and is rejected below. We must propagate the error (no
            // longer ignore a failed DEL): if the consume silently failed the
            // approval would survive and the single-use guarantee would be
            // voided.
            let consumed_approval: Option<String> = self
                .client
                .getdel(&gate_key)
                .await
                .context("Redis GETDEL failed while consuming gate approval")?;

            if consumed_approval.is_none() {
                bail!(
                    "Cannot transition from '{}' to '{}' without SENTINEL approval.\n\
                     SENTINEL must run: openflows-harness gate approve --phase {}\n\
                     This ensures your plan is reviewed before implementation begins.",
                    source_phase,
                    phase,
                    source_phase
                );
            }

            info!(
                ticket,
                from = source_phase,
                to = phase,
                "Gate approval verified and consumed (GETDEL), allowing transition"
            );
        }

        let val = serde_json::json!({
            "phase": phase,
            "role": role,
            "ts": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        });
        let _: Result<(), _> = self
            .client
            .set::<(), _, _>(&status_key, val.to_string(), None, None, false)
            .await;
        println!("Wrote: {}", status_key);
        info!(key = %status_key, phase, "status set");
        Ok(())
    }

    /// Approve a gated phase transition (SENTINEL only).
    ///
    /// This allows FORGE to proceed past a gated phase (e.g., `planning` → `building`).
    ///
    /// Only the SENTINEL role may approve a gate. FORGE cannot approve its own
    /// plan — if it could, the mandatory review checkpoint would be bypassable by
    /// the very agent it is meant to supervise. The worker CLI (`openflows-harness
    /// gate approve`) derives `role` from `OPENFLOWS_ROLE`, so only a SENTINEL
    /// workspace can satisfy this; the admin `openflows gate approve` CLI passes
    /// `--approver` (defaulting to SENTINEL). Any other role is rejected.
    pub async fn gate_approve(
        &self,
        ticket: &str,
        role: &str,
        phase: &str,
        notes: Option<&str>,
    ) -> Result<()> {
        authorize_gate_approver(role)?;
        if !GATED_PHASES.contains(&phase) {
            bail!(
                "Phase '{}' is not a gated phase. Gated phases: {}",
                phase,
                GATED_PHASES.join(", ")
            );
        }

        // Verify current phase matches
        let status_key = self.key(&full_ticket_key_flat(ticket, "status"));
        let current_status: Option<String> = self
            .client
            .get(&status_key)
            .await
            .context("Redis GET failed")?;

        if let Some(ref status_json) = current_status {
            if let Ok(status) = serde_json::from_str::<serde_json::Value>(status_json) {
                if let Some(current_phase) = status.get("phase").and_then(|p| p.as_str()) {
                    if current_phase != phase {
                        bail!(
                            "Cannot approve '{}' gate — current phase is '{}'. \
                             FORGE must be in the '{}' phase to receive approval.",
                            phase,
                            current_phase,
                            phase
                        );
                    }
                }
            }
        } else {
            bail!(
                "No status found for ticket {}. FORGE must set status to '{}' first.",
                ticket,
                phase
            );
        }

        let approval = GateApproval {
            phase: phase.to_string(),
            approved_by: role.to_string(),
            ts: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs(),
            notes: notes.map(|s| s.to_string()),
        };

        let gate_key = self.key(&format!("ticket:{}:gate:{}", ticket, phase));
        let json = serde_json::to_string(&approval)?;
        let _: Result<(), _> = self
            .client
            .set::<(), _, _>(&gate_key, json, None, None, false)
            .await;

        println!("Gate approved: {} phase '{}' by {}", ticket, phase, role);
        info!(key = %gate_key, phase, role, "gate approved");
        Ok(())
    }

    /// Check if a gated phase has been approved.
    pub async fn gate_status(&self, ticket: &str, phase: &str) -> Result<()> {
        let gate_key = self.key(&format!("ticket:{}:gate:{}", ticket, phase));
        let approval: Option<String> = self
            .client
            .get(&gate_key)
            .await
            .context("Redis GET failed")?;

        match approval {
            Some(json) => {
                let approval: GateApproval =
                    serde_json::from_str(&json).context("Failed to parse gate approval")?;
                println!(
                    "✓ Gate '{}' approved by {} at {}",
                    approval.phase, approval.approved_by, approval.ts
                );
                if let Some(notes) = approval.notes {
                    println!("  Notes: {}", notes);
                }
            }
            None => {
                println!("✗ Gate '{}' not yet approved", phase);
            }
        }
        Ok(())
    }

    /// Read the current status JSON for this ticket. Prints `{}` when unset
    /// so hook scripts can always parse the output.
    pub async fn status_get(&self, ticket: &str) -> Result<()> {
        let key = self.key(&full_ticket_key_flat(ticket, "status"));
        let val: Option<String> = self.client.get(&key).await.context("Redis GET failed")?;
        println!("{}", val.unwrap_or_else(|| "{}".to_string()));
        debug!(key = %key, "status read");
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
        let _: Result<(), _> = self
            .client
            .set::<(), _, _>(&key, json, None, None, false)
            .await;
        println!("Wrote: {}", key);
        info!(key = %key, "handoff written");
        Ok(())
    }

    /// Record that a PR was opened.
    pub async fn pr_opened(&self, ticket: &str, pr: &u64, branch: &str, title: &str) -> Result<()> {
        let payload = PrInfo {
            pr_number: *pr,
            branch: branch.to_string(),
            title: title.to_string(),
        };
        let key = self.key(&full_ticket_key_flat(ticket, "pr"));
        let json = serde_json::to_string(&payload)?;
        let _: Result<(), _> = self
            .client
            .set::<(), _, _>(&key, json, None, None, false)
            .await;
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
    ) -> Result<()> {
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
        let payload = ReviewPayload {
            verdict: verdict.to_string(),
            report,
            pr_number: pr,
        };
        // `review submit` records the SENTINEL PR/final review verdict. It is
        // namespaced by review type (`pr_review`) so it never collides with the
        // planning-gate review state (`ticket:{id}:gate:planning`).
        let key = self.key(&review_verdict_key(ticket, REVIEW_TYPE_PR));
        let json = serde_json::to_string(&payload)?;
        let _: Result<(), _> = self
            .client
            .set::<(), _, _>(&key, json, None, None, false)
            .await;
        println!("Wrote: {} (verdict: {})", key, verdict);
        info!(key = %key, verdict, "review submitted");
        Ok(())
    }

    /// Record that a merge completed (vessel).
    pub async fn merge_done(&self, ticket: &str, pr: &u64, sha: &str) -> Result<()> {
        let payload = MergePayload {
            pr_number: *pr,
            sha: sha.to_string(),
            merged: true,
        };
        let key = self.key(&full_ticket_key_flat(ticket, "deployment"));
        let json = serde_json::to_string(&payload)?;
        let _: Result<(), _> = self
            .client
            .set::<(), _, _>(&key, json, None, None, false)
            .await;
        println!("Wrote: {} (pr #{}, merged)", key, pr);
        info!(key = %key, pr, "merge done");
        Ok(())
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
            let _: Result<(), _> = self
                .client
                .set::<(), _, _>(
                    &key,
                    &json,
                    Some(fred::types::Expiration::EX(120)),
                    None,
                    false,
                )
                .await;
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
                    let fail = a2a_protocol::VerifyResult {
                        task_id: task_id.clone(),
                        exit_code: None,
                        timed_out: false,
                        duration_ms: 0,
                        stdout_ref: format!("audit:a2a:{}:stdout", task_id),
                        stderr_ref: format!("audit:a2a:{}:stderr", task_id),
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
    /// FORGE writes PLAN.md as a local file in its workspace, then calls this
    /// to persist it directly to Redis SharedStore so SENTINEL (and NEXUS)
    /// can read it without relying on Coder API filesystem access.
    pub async fn plan_write(&self, ticket: &str, file_path: &Path) -> Result<()> {
        let content = std::fs::read_to_string(file_path)
            .context(format!("Failed to read plan file: {}", file_path.display()))?;

        let key = self.key(&format!("pair:{}:plan", ticket));
        let _: Result<(), _> = self
            .client
            .set::<(), _, _>(&key, content, None, None, false)
            .await;

        println!("Wrote: {}", key);
        info!(key = %key, "plan written to SharedStore");
        Ok(())
    }

    /// Read a plan artifact from Redis (`pair:{id}:plan`) and print to stdout.
    ///
    /// SENTINEL uses this to retrieve the FORGE plan during planning gate review.
    /// Prints the plan content as raw markdown; prints nothing if unset.
    pub async fn plan_read(&self, ticket: &str) -> Result<()> {
        let key = self.key(&format!("pair:{}:plan", ticket));
        let val: Option<String> = self.client.get(&key).await.context("Redis GET failed")?;
        match val {
            Some(content) => {
                print!("{}", content);
            }
            None => {
                bail!(
                    "No plan found for ticket {}. FORGE must write a plan via \
                     `openflows-harness plan write --file PLAN.md` first.",
                    ticket
                );
            }
        }
        debug!(key = %key, "plan read");
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
    fn test_valid_phases() {
        assert!(VALID_PHASES.contains(&"planning"));
        assert!(VALID_PHASES.contains(&"building"));
        assert!(!VALID_PHASES.contains(&"invalid_phase"));
    }

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

    #[test]
    fn test_first_status_must_enter_via_planning_or_blocked() {
        // A brand-new ticket must enter through the planning gate (or report
        // an immediate blocker via `blocked`). It cannot short-circuit
        // straight to building/testing/review_ready and skip the SENTINEL plan
        // review.
        assert!(is_allowed_first_phase("planning"));
        assert!(is_allowed_first_phase("blocked"));
        assert!(!is_allowed_first_phase("building"));
        assert!(!is_allowed_first_phase("testing"));
        assert!(!is_allowed_first_phase("review_ready"));
    }

    #[test]
    fn test_gate_source_only_for_leaving_planning() {
        // Leaving planning -> any other phase requires (and consumes) an
        // approval sourced from the planning gate.
        assert_eq!(
            gate_source_for_transition("planning", "building"),
            Some("planning")
        );
        assert_eq!(
            gate_source_for_transition("planning", "testing"),
            Some("planning")
        );
        assert_eq!(
            gate_source_for_transition("planning", "review_ready"),
            Some("planning")
        );

        // Staying in planning is a no-op, not a gated transition.
        assert_eq!(gate_source_for_transition("planning", "planning"), None);

        // Transitions among downstream phases do not require re-approval.
        assert_eq!(gate_source_for_transition("building", "testing"), None);
        assert_eq!(gate_source_for_transition("testing", "review_ready"), None);

        // Returning TO planning is free; the next outbound transition is
        // gated, and because the previous approval was consumed on the way
        // out, the revised plan forces a fresh SENTINEL approval.
        assert_eq!(gate_source_for_transition("building", "planning"), None);
        assert_eq!(gate_source_for_transition("blocked", "planning"), None);
    }
}
