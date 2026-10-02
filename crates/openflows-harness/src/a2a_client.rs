// crates/openflows-harness/src/a2a_client.rs
//! A2A HTTP client for harness workers to communicate with nexus relay.
//!
//! Used by both Sentinel (verify request) and Forge (verify serve) roles.
//! The client is a thin wrapper around reqwest that handles JSON-RPC
//! envelope format expected by the A2A relay.

use a2a_protocol::{VerifyProgressEvent, VerifyRequest, VerifyResult};
use anyhow::{anyhow, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tracing::{debug, info, warn};

/// A2A relay client for communicating with the nexus-hosted relay.
#[allow(dead_code)]
pub struct A2AClient {
    http_client: reqwest::Client,
    relay_url: String,
    pair_id: String,
    role: String, // "sentinel" or "forge"
    pair_token: String,
}

impl A2AClient {
    /// Create a new A2A client pointing at the nexus relay.
    ///
    /// The relay address is read from `A2A_RELAY_ADDR` env var, which the
    /// workspace template must inject with the nexus relay's network address
    /// (in the Coder docker deployment: `openflows-nexus:3000`). The
    /// loopback fallback is only a local-testing convenience — a provisioned
    /// workspace that omits `A2A_RELAY_ADDR` would otherwise silently target
    /// its own interface (issue #143 / PR review), so it warns here.
    pub fn new(pair_id: String, role: String) -> Result<Self> {
        let env = config::EnvConfig::from_env()?;
        let relay_addr = match env.infra.a2a_relay_addr.as_str() {
            addr if !addr.trim().is_empty() => addr.to_string(),
            _ => {
                warn!(
                    "A2A_RELAY_ADDR is not set; defaulting to 127.0.0.1:3000. \
                     Provisioned workspaces must set A2A_RELAY_ADDR to the nexus relay \
                     (e.g. openflows-nexus:3000) — loopback will not reach the relay."
                );
                "127.0.0.1:3000".to_string()
            }
        };
        let relay_url = format!("http://{}", relay_addr);

        let http_client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;
        let pair_token = std::env::var("A2A_PAIR_TOKEN")
            .ok()
            .filter(|v| !v.trim().is_empty())
            .context("A2A_PAIR_TOKEN is not set; reprovision Sentinel/FORGE workspaces so the relay can authenticate pair-scoped verification")?;

        Ok(Self {
            http_client,
            relay_url,
            pair_id,
            role,
            pair_token,
        })
    }

    /// Check if the relay is healthy.
    pub async fn health_check(&self) -> Result<()> {
        let url = format!("{}/health", self.relay_url);
        let response = self.http_client.get(&url).send().await?;
        if response.status().is_success() {
            info!("A2A relay health check passed");
            Ok(())
        } else {
            Err(anyhow!("A2A relay unhealthy: {}", response.status()))
        }
    }

    /// Submit a verify request (Sentinel-side, task 3).
    /// Sends message/send RPC to relay, gets back task_id.
    pub async fn submit_verify_request(&self, req: &VerifyRequest) -> Result<String> {
        let rpc_request = json!({
            "jsonrpc": "2.0",
            "method": "message/send",
            "params": {
                "message": {
                    "task": req,
                },
                "pair_token": self.pair_token,
            },
            "id": uuid::Uuid::new_v4().to_string(),
        });

        let url = format!("{}/rpc", self.relay_url);
        let response = self
            .http_client
            .post(&url)
            .json(&rpc_request)
            .send()
            .await
            .context("Failed to send verify request to relay")?;

        let body: Value = response.json().await?;

        // Parse JSON-RPC response
        if let Some(error) = body.get("error").and_then(|e| e.as_object()) {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(anyhow!("A2A RPC error: {}", msg));
        }

        let task_id = body
            .get("result")
            .and_then(|r| r.get("task_id"))
            .and_then(|t| t.as_str())
            .ok_or_else(|| anyhow!("No task_id in response"))?
            .to_string();

        debug!(task_id = %task_id, "Verify request submitted");
        Ok(task_id)
    }

    /// Get a task's current raw status string (e.g. "pending", "running",
    /// "completed", "cancelled"). Used by the cancel-token poller.
    pub async fn get_task_status_str(&self, task_id: &str) -> Result<String> {
        let rpc_request = json!({
            "jsonrpc": "2.0",
            "method": "tasks/get",
            "params": {
                "task_id": task_id,
                "pair_id": self.pair_id,
                "pair_token": self.pair_token,
            },
            "id": uuid::Uuid::new_v4().to_string(),
        });

        let url = format!("{}/rpc", self.relay_url);
        let response = self
            .http_client
            .post(&url)
            .json(&rpc_request)
            .send()
            .await
            .context("Failed to get task status from relay")?;

        let body: Value = response.json().await?;
        let result = body.get("result");
        Ok(result
            .and_then(|r| r.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("unknown")
            .to_string())
    }

    /// Get a task's current status (Sentinel polling after submit).
    pub async fn get_task_status(&self, task_id: &str) -> Result<Option<VerifyResult>> {
        let rpc_request = json!({
            "jsonrpc": "2.0",
            "method": "tasks/get",
            "params": {
                "task_id": task_id,
                "pair_id": self.pair_id,
                "pair_token": self.pair_token,
            },
            "id": uuid::Uuid::new_v4().to_string(),
        });

        let url = format!("{}/rpc", self.relay_url);
        let response = self
            .http_client
            .post(&url)
            .json(&rpc_request)
            .send()
            .await
            .context("Failed to get task status from relay")?;

        let body: Value = response.json().await?;

        if let Some(error) = body.get("error").and_then(|e| e.as_object()) {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(anyhow!("A2A RPC error: {}", msg));
        }

        let result = body.get("result");
        let status = result
            .and_then(|r| r.get("status"))
            .and_then(|s| s.as_str())
            .unwrap_or("unknown");

        if status == "completed" {
            let verify_result: VerifyResult = result
                .and_then(|r| r.get("result").cloned())
                .map(serde_json::from_value)
                .transpose()?
                .context("completed task has no serializable result")?;
            return Ok(Some(verify_result));
        }

        Ok(None)
    }

    /// Claim the next pending task for this pair (Forge executor role).
    /// Returns the claimed `VerifyRequest` (with its task_id via a wrapper)
    /// or `None` when no task is pending for the pair.
    pub async fn claim_next_task(&self) -> Result<Option<(String, VerifyRequest)>> {
        let rpc_request = json!({
            "jsonrpc": "2.0",
            "method": "tasks/claim",
            "params": {
                "pair_id": self.pair_id,
                "role": self.role,
                "checkout_version": 2,
                "pair_token": self.pair_token,
            },
            "id": uuid::Uuid::new_v4().to_string(),
        });

        let url = format!("{}/rpc", self.relay_url);
        let response = self
            .http_client
            .post(&url)
            .json(&rpc_request)
            .send()
            .await
            .context("Failed to claim task from relay")?;

        let body: Value = response.json().await?;

        if let Some(error) = body.get("error").and_then(|e| e.as_object()) {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(anyhow!("A2A RPC error: {}", msg));
        }

        let task = body.get("result").cloned().unwrap_or(Value::Null);
        if task.is_null() {
            return Ok(None);
        }

        let task_id = task
            .get("task_id")
            .and_then(|t| t.as_str())
            .context("claimed task missing task_id")?
            .to_string();
        let request: VerifyRequest = serde_json::from_value(
            task.get("request")
                .cloned()
                .context("claimed task missing request")?,
        )?;

        debug!(task_id = %task_id, "Task claimed");
        Ok(Some((task_id, request)))
    }

    /// Submit a terminal result for a claimed task (Forge executor role).
    pub async fn complete_task(&self, result: &VerifyResult) -> Result<()> {
        let rpc_request = json!({
            "jsonrpc": "2.0",
            "method": "tasks/complete",
            "params": {
                "task_id": result.task_id,
                "pair_id": self.pair_id,
                "pair_token": self.pair_token,
                "result": result,
            },
            "id": uuid::Uuid::new_v4().to_string(),
        });

        let url = format!("{}/rpc", self.relay_url);
        let response = self
            .http_client
            .post(&url)
            .json(&rpc_request)
            .send()
            .await
            .context("Failed to complete task on relay")?;

        let body: Value = response.json().await?;

        if let Some(error) = body.get("error").and_then(|e| e.as_object()) {
            let msg = error
                .get("message")
                .and_then(|m| m.as_str())
                .unwrap_or("unknown error");
            return Err(anyhow!("A2A RPC error: {}", msg));
        }

        debug!(task_id = %result.task_id, "Task completion submitted");
        Ok(())
    }

    /// Cancel a running task (Sentinel-side).
    /// The client's pair_id is sent with the request so the relay can
    /// verify that the cancelling workspace owns the task (pair-scoped).
    pub async fn cancel_task(&self, task_id: &str) -> Result<()> {
        let rpc_request = json!({
            "jsonrpc": "2.0",
            "method": "tasks/cancel",
            "params": {
                "task_id": task_id,
                "pair_id": self.pair_id,
                "pair_token": self.pair_token,
            },
            "id": uuid::Uuid::new_v4().to_string(),
        });

        let url = format!("{}/rpc", self.relay_url);
        let _response = self
            .http_client
            .post(&url)
            .json(&rpc_request)
            .send()
            .await
            .context("Failed to cancel task")?;

        debug!(task_id = %task_id, "Task cancellation sent");
        Ok(())
    }

    /// Push a progress event to the relay (Forge executor side).
    /// Streams stdout/stderr chunks in real-time during execution.
    pub async fn push_progress(&self, task_id: &str, event: &VerifyProgressEvent) -> Result<()> {
        let rpc_request = json!({
            "jsonrpc": "2.0",
            "method": "tasks/push_progress",
            "params": {
                "task_id": task_id,
                "pair_id": self.pair_id,
                "pair_token": self.pair_token,
                "stream": "stdout",
                "chunk": "",
            },
            "id": uuid::Uuid::new_v4().to_string(),
        });

        // Merge the progress event fields into params
        let mut rpc = rpc_request;
        if let Some(obj) = rpc.get_mut("params").and_then(|p| p.as_object_mut()) {
            if let Ok(event_val) = serde_json::to_value(event) {
                if let Some(event_obj) = event_val.as_object() {
                    for (k, v) in event_obj {
                        obj.insert(k.clone(), v.clone());
                    }
                }
            }
        }

        let url = format!("{}/rpc", self.relay_url);
        let response = self
            .http_client
            .post(&url)
            .json(&rpc)
            .send()
            .await
            .context("Failed to push progress event")?;

        let _body: Value = response.json().await?;
        debug!(task_id = %task_id, "Progress event pushed");
        Ok(())
    }

    /// Subscribe to SSE progress events for a task (Sentinel side).
    /// Spawns a background task that reads the SSE stream and forwards events
    /// to the returned `UnboundedReceiver`. The caller can await on this
    /// receiver to get progress events as they arrive.
    pub async fn subscribe_sse(
        &self,
        task_id: &str,
    ) -> Result<tokio::sync::mpsc::UnboundedReceiver<VerifyProgressEvent>> {
        let url = format!("{}/?task_id={}", self.relay_url, task_id);

        let response = self
            .http_client
            .get(&url)
            .send()
            .await
            .context("Failed to connect to SSE stream")?;

        if !response.status().is_success() {
            let status = response.status();
            return Err(anyhow!("SSE connection failed with status {}", status));
        }

        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<VerifyProgressEvent>();
        let task_id_owned = task_id.to_string();

        // Spawn a background reader task that parses SSE events
        tokio::spawn(async move {
            use futures::StreamExt;
            let mut stream = response.bytes_stream();
            while let Some(chunk_result) = stream.next().await {
                let chunk = match chunk_result {
                    Ok(c) => c,
                    Err(e) => {
                        warn!(error = %e, task_id = %task_id_owned, "SSE stream error");
                        break;
                    }
                };

                let text = String::from_utf8_lossy(&chunk);
                let mut buffer = String::new();

                for line in text.lines() {
                    if let Some(data) = line.strip_prefix("data: ") {
                        buffer.push_str(data);
                    } else if let Some(data) = line.strip_prefix("data:") {
                        buffer.push_str(data);
                    } else if line.is_empty() && !buffer.is_empty() {
                        if let Ok(event) = serde_json::from_str::<VerifyProgressEvent>(&buffer) {
                            let _ = tx.send(event);
                        }
                        buffer.clear();
                    }
                }
                if !buffer.is_empty() {
                    if let Ok(event) = serde_json::from_str::<VerifyProgressEvent>(&buffer) {
                        let _ = tx.send(event);
                    }
                }
            }
            debug!(task_id = %task_id_owned, "SSE stream ended");
        });

        Ok(rx)
    }
}
