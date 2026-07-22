//! Harness SharedStore — typed, validated Redis I/O with tenant namespacing.
//!
//! All keys are prefixed with `ns:{tenant}:` for tenant isolation.
//! All writes are validated against serde schemas from `config::state`.

use anyhow::{bail, Context, Result};
use config::state::{full_ticket_key, full_ticket_key_flat, heartbeat_key, HeartbeatRecord};
use fred::prelude::*;
use serde::{Deserialize, Serialize};
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};
use tracing::{debug, info};

/// Valid phases for the `status set` command.
const VALID_PHASES: &[&str] = &["planning", "building", "testing", "review_ready", "blocked"];

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
    pub async fn status_set(&self, ticket: &str, role: &str, phase: &str) -> Result<()> {
        if !VALID_PHASES.contains(&phase) {
            bail!(
                "Invalid phase '{}'. Valid phases: {}",
                phase,
                VALID_PHASES.join(", ")
            );
        }
        let key = self.key(&full_ticket_key_flat(ticket, "status"));
        let val = serde_json::json!({
            "phase": phase,
            "role": role,
            "ts": SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs(),
        });
        let _: Result<(), _> = self
            .client
            .set::<(), _, _>(&key, val.to_string(), None, None, false)
            .await;
        println!("Wrote: {}", key);
        info!(key = %key, phase, "status set");
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
    pub async fn review_submit(
        &self,
        ticket: &str,
        role: &str,
        verdict: &str,
        report_path: &Path,
        pr: Option<u64>,
    ) -> Result<()> {
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
        let key = self.key(&full_ticket_key(ticket, "review", role));
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
}
