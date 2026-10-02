// crates/agent-nexus/src/a2a/routing.rs
//! Pair-scoped routing table and session management for the A2A relay.
//!
//! Maintains connections from Sentinel and Forge workspaces, keyed by
//! `(pair_id, role)`. Routes verify requests from Sentinel to the
//! corresponding Forge executor.

use a2a_protocol::{VerifyProgressEvent, VerifyRequest, VerifyResult};
use anyhow::{anyhow, Result};
use pocketflow_core::SharedStore;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use tokio::sync::{broadcast, Mutex, RwLock};
use tracing::{debug, warn};

/// A2A relay session for a single connected workspace. Holds task queue
/// and result channel.
#[derive(Clone)]
pub struct A2ASession {
    pub pair_id: String,
    pub role: String, // "sentinel" or "forge"
    pub workspace_id: String, // For audit; e.g., "forge-T-048"
                      // TODO: event channel for streaming progress back to workspace
}

/// Lifecycle state of a single A2A `verify` task as it moves
/// Sentinel → relay → Forge → relay → Sentinel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskState {
    /// Submitted by Sentinel, awaiting assignment to a Forge executor.
    Pending,
    /// Claimed by a Forge executor, currently running.
    Running,
    /// Forge submitted a terminal result; mirrored to Redis.
    Completed,
    /// Cancelled by Sentinel or Forge.
    Cancelled,
}

/// A single buffered progress event with sequence number (for resubscribe).
#[derive(Debug, Clone, Serialize)]
pub struct BufferedEvent {
    pub seq: u64,
    pub event: VerifyProgressEvent,
    pub timestamp: i64,
}

/// Bounded event buffer per task (max 1000 events or ~1 MiB, whichever smaller).
/// Eviction is FIFO.
#[derive(Debug, Clone)]
pub struct EventBuffer {
    events: VecDeque<BufferedEvent>,
    next_seq: u64,
    max_events: usize,
    max_bytes: usize,
    current_bytes: usize,
}

impl EventBuffer {
    const MAX_EVENTS: usize = 1000;
    const MAX_BYTES: usize = 1_048_576; // 1 MiB

    pub fn new() -> Self {
        Self {
            events: VecDeque::with_capacity(Self::MAX_EVENTS),
            next_seq: 0,
            max_events: Self::MAX_EVENTS,
            max_bytes: Self::MAX_BYTES,
            current_bytes: 0,
        }
    }

    /// Push a progress event into the buffer, evicting oldest events if
    /// capacity is exceeded.
    pub fn push(&mut self, event: VerifyProgressEvent) -> BufferedEvent {
        let event_size = serde_json::to_string(&event).map(|s| s.len()).unwrap_or(0);
        let seq = self.next_seq;
        self.next_seq += 1;

        let buffered = BufferedEvent {
            seq,
            event,
            timestamp: chrono::Utc::now().timestamp(),
        };

        self.events.push_back(buffered.clone());
        self.current_bytes += event_size;

        // Evict FIFO while over capacity
        while self.events.len() > self.max_events || self.current_bytes > self.max_bytes {
            if let Some(evicted) = self.events.pop_front() {
                let evicted_size = serde_json::to_string(&evicted.event)
                    .map(|s| s.len())
                    .unwrap_or(0);
                self.current_bytes = self.current_bytes.saturating_sub(evicted_size);
            }
        }

        buffered
    }

    /// Return all events with seq >= `last_seq`.
    /// Pass `last_seq = 0` to get all events (event sequence numbers start at 0).
    pub fn events_since(&self, last_seq: u64) -> Vec<BufferedEvent> {
        self.events
            .iter()
            .filter(|e| e.seq >= last_seq)
            .cloned()
            .collect()
    }

    /// Current highest sequence number (0 if no events).
    pub fn current_seq(&self) -> u64 {
        self.next_seq.saturating_sub(1)
    }

    /// Maximum number of events the buffer can hold before FIFO eviction.
    pub fn max_events(&self) -> usize {
        self.max_events
    }

    /// Maximum bytes the buffer can hold before FIFO eviction.
    pub fn max_bytes(&self) -> usize {
        self.max_bytes
    }
}

impl Default for EventBuffer {
    fn default() -> Self {
        Self::new()
    }
}

/// Pair-scoped task entry, keyed by idempotency hash.
#[derive(Clone)]
pub struct TaskEntry {
    pub task_id: String,
    pub request: VerifyRequest,
    pub idempotency_key: String,
    pub requester: String,
    pub state: TaskState,
    pub result: Option<VerifyResult>,
}

/// Central A2A relay hosted by nexus. Manages pair-scoped routing,
/// command allowlist validation, idempotency dedup, and result mirroring
/// to Redis.
pub struct A2ARelay {
    store: Arc<SharedStore>,
    // (pair_id, role) → connected session
    sessions: Arc<RwLock<HashMap<(String, String), A2ASession>>>,
    // task_id → task entry (claim/complete/get by task_id)
    tasks: Arc<Mutex<HashMap<String, TaskEntry>>>,
    // idempotency_key → task_id (dedup: (pair_id, sha256(request_body)))
    idempotency: Arc<Mutex<HashMap<String, String>>>,
    // Timestamps for idempotency TTL eviction (idempotency_key → inserted_at)
    idempotency_ts: Arc<Mutex<HashMap<String, std::time::Instant>>>,
    // task_id → progress event buffer (for resubscribe / SSE replay)
    event_buffers: Arc<Mutex<HashMap<String, EventBuffer>>>,
    // task_id → broadcast sender for real-time progress streaming
    broadcast_senders: Arc<RwLock<HashMap<String, broadcast::Sender<BufferedEvent>>>>,
    // task_id → cancel flag (set by tasks/cancel)
    cancel_tokens: Arc<Mutex<HashMap<String, Arc<AtomicBool>>>>,
}

impl A2ARelay {
    /// Create a new A2A relay.
    pub fn new(store: Arc<SharedStore>) -> Self {
        Self {
            store,
            sessions: Arc::new(RwLock::new(HashMap::new())),
            tasks: Arc::new(Mutex::new(HashMap::new())),
            idempotency: Arc::new(Mutex::new(HashMap::new())),
            idempotency_ts: Arc::new(Mutex::new(HashMap::new())),
            event_buffers: Arc::new(Mutex::new(HashMap::new())),
            broadcast_senders: Arc::new(RwLock::new(HashMap::new())),
            cancel_tokens: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Register a connected workspace session.
    pub async fn register_session(
        &self,
        pair_id: String,
        role: String,
        workspace_id: String,
    ) -> Result<()> {
        let key = (pair_id.clone(), role.clone());
        let session = A2ASession {
            pair_id: pair_id.clone(),
            role: role.clone(),
            workspace_id,
        };
        self.sessions.write().await.insert(key.clone(), session);
        debug!(pair_id = %key.0, role = %key.1, "Session registered");
        Ok(())
    }

    /// Deregister a connected workspace session.
    pub async fn deregister_session(&self, pair_id: &str, role: &str) -> Result<()> {
        let key = (pair_id.to_string(), role.to_string());
        self.sessions.write().await.remove(&key);
        debug!(pair_id, role, "Session deregistered");
        Ok(())
    }

    /// Get a registered session for a given pair and role.
    pub async fn get_session(&self, pair_id: &str, role: &str) -> Option<A2ASession> {
        let key = (pair_id.to_string(), role.to_string());
        self.sessions.read().await.get(&key).cloned()
    }

    /// Validate a verify request: check allowlist, cwd, pair_id match.
    /// Returns error if validation fails (caller logs to audit:a2a:rejected).
    ///
    /// Validation rules (task 2.2):
    /// 1. Pair IDs must match (Sentinel can only submit requests for its own pair)
    /// 2. Command must be in the allowlist
    /// 3. cwd must be one of the allowed values ("repo", "worktree")
    /// 4. timeout_secs must be reasonable (not negative, not excessive)
    pub fn validate_verify_request(
        &self,
        req: &VerifyRequest,
        requester_pair_id: &str,
    ) -> Result<()> {
        // Rule 1: Requester and request must agree on pair_id. In v1 the
        // requester is caller-supplied (self-declared) — this check prevents
        // accidental mismatches between a misconfigured client's pair_id and
        // the pair_id it writes into the request body.  It is NOT a
        // cryptographic authorization: a workspace on the Docker network
        // can supply any value here.
        //
        // TODO(v2): verify requester_pair_id against a workspace identity
        // token (plan task 2) so the relay can enforce pair-scoped access
        // without trusting self-declared values.
        if req.pair_id != requester_pair_id {
            return Err(anyhow!(
                "pair_id mismatch: request says {}, requester is {}",
                req.pair_id,
                requester_pair_id
            ));
        }

        // Rule 2: Command allowlist
        a2a_protocol::validate_command_argv(&req.argv)?;

        // Rule 3: cwd validation
        // Only "repo" and "worktree" are allowed; nothing else.
        // The actual path traversal check (doesn't escape pair root) happens
        // in the executor (task 5), since nexus doesn't have workspace FS.
        use a2a_protocol::VerifyCwd;
        match req.cwd {
            VerifyCwd::Repo | VerifyCwd::Worktree => {} // If serde can't parse VerifyCwd, this is already rejected
                                                        // by serde deserialization, not by this function.
        }

        // Rule 4: timeout_secs sanity check
        const MAX_TIMEOUT_SECS: u64 = 3600; // 1 hour is the hard limit
        if req.timeout_secs == 0 {
            return Err(anyhow!("timeout_secs must be > 0"));
        }
        if req.timeout_secs > MAX_TIMEOUT_SECS {
            return Err(anyhow!(
                "timeout_secs exceeds maximum ({})",
                MAX_TIMEOUT_SECS
            ));
        }

        Ok(())
    }

    /// Check idempotency: if a task with the same key was recently
    /// submitted, return its task_id. Otherwise, generate a new task_id
    /// and record it.
    pub async fn check_or_create_task(
        &self,
        req: &VerifyRequest,
        requester: &str,
    ) -> Result<String> {
        let idempotency_key = req.idempotency_seed()?;

        // Serialize lookup, terminal replacement, and insertion with task state.
        // All multi-map operations acquire tasks before idempotency before ts.
        let mut tasks = self.tasks.lock().await;
        let mut idempotency = self.idempotency.lock().await;
        if let Some(task_id) = idempotency.get(&idempotency_key) {
            if tasks
                .get(task_id)
                .is_some_and(|task| matches!(task.state, TaskState::Pending | TaskState::Running))
            {
                return Ok(task_id.clone());
            }
        }

        // Generate new task_id (uuid v4)
        let task_id = uuid::Uuid::new_v4().to_string();

        let entry = TaskEntry {
            task_id: task_id.clone(),
            request: req.clone(),
            idempotency_key: idempotency_key.clone(),
            requester: requester.to_string(),
            state: TaskState::Pending,
            result: None,
        };

        tasks.insert(task_id.clone(), entry);
        idempotency.insert(idempotency_key.clone(), task_id.clone());
        self.idempotency_ts
            .lock()
            .await
            .insert(idempotency_key, std::time::Instant::now());

        drop(idempotency);

        // Mirror the request to the audit trail so it can be replayed later
        let _ = self.mirror_request(&task_id, req).await;

        // Initialize event buffer and broadcast channel for this task
        self.event_buffers
            .lock()
            .await
            .entry(task_id.clone())
            .or_default();
        let (tx, _) = broadcast::channel(256);
        self.broadcast_senders
            .write()
            .await
            .insert(task_id.clone(), tx);

        debug!(
            task_id = %task_id,
            pair_id = %req.pair_id,
            "New task created"
        );
        Ok(task_id)
    }

    /// Claim the next pending task assigned to a given pair. Marks it
    /// `Running` and returns it to the claiming executor role (Forge).
    /// Returns `None` when no pending task remains for the pair.
    pub async fn claim_next_task(&self, pair_id: &str) -> Result<Option<TaskEntry>> {
        let mut tasks = self.tasks.lock().await;
        let task_id = tasks
            .iter()
            .find(|(_, t)| t.request.pair_id == pair_id && t.state == TaskState::Pending)
            .map(|(id, _)| id.clone());

        let entry = match task_id {
            Some(id) => {
                let entry = tasks.get(&id).unwrap().clone();
                tasks.get_mut(&id).unwrap().state = TaskState::Running;
                entry
            }
            None => return Ok(None),
        };

        debug!(task_id = %entry.task_id, pair_id, "Task claimed by executor");
        Ok(Some(entry))
    }

    /// Mark a task Completed and store its terminal result, mirroring it
    /// to Redis per the plan (result must be durable before Sentinel sees
    /// it). Returns an error if the task is unknown.
    pub async fn complete_task(&self, task_id: &str, result: VerifyResult) -> Result<()> {
        let mut tasks = self.tasks.lock().await;
        let entry = tasks
            .get_mut(task_id)
            .ok_or_else(|| anyhow!("task not found: {}", task_id))?;
        entry.state = TaskState::Completed;
        entry.result = Some(result.clone());

        // Mirror the terminal result to Redis before releasing the lock so a
        // concurrent `tasks/get` from Sentinel never observes a completed task
        // without a durable result.
        self.mirror_result(&result).await?;

        debug!(task_id, "Task completed and result mirrored");
        Ok(())
    }

    /// Get a task's current state and terminal result (if any).
    pub async fn get_task_state(&self, task_id: &str) -> Option<TaskState> {
        let tasks = self.tasks.lock().await;
        tasks.get(task_id).map(|t| t.state)
    }

    /// Look up a task's request (used by Forge to execute it after claiming)
    /// and its terminal result once complete.
    pub async fn get_task(&self, task_id: &str) -> Option<TaskEntry> {
        let tasks = self.tasks.lock().await;
        tasks.get(task_id).cloned()
    }

    /// Mirror a terminal task result to Redis before acking completion.
    /// Writes to:
    /// - `pair:{pair_id}:verification` (latest result)
    /// - `audit:a2a:{task_id}:result` (immutable result artifact)
    /// - `audit:a2a:{task_id}:request` (original request for replay)
    pub async fn mirror_result(&self, result: &VerifyResult) -> Result<()> {
        let verification_key = a2a_protocol::verification_key(&result.executor.workspace);

        // Mirror to pair:{pair_id}:verification
        self.store
            .set(&verification_key, serde_json::to_value(result)?)
            .await;

        // Mirror to audit:a2a:{task_id}:result
        let audit_result_key = format!("{}:result", a2a_protocol::audit_task_key(&result.task_id));
        self.store
            .set(&audit_result_key, serde_json::to_value(result)?)
            .await;

        debug!(
            task_id = %result.task_id,
            pair_id = %result.executor.workspace,
            "Result mirrored to Redis"
        );
        Ok(())
    }

    /// Mirror the original verify request to the audit trail so
    /// `get_task_request` can replay it later. Call this once when a
    /// new task is created.
    pub async fn mirror_request(&self, task_id: &str, request: &VerifyRequest) -> Result<()> {
        let audit_request_key = format!("{}:request", a2a_protocol::audit_task_key(task_id));
        self.store
            .set(&audit_request_key, serde_json::to_value(request)?)
            .await;
        debug!(task_id, "Request mirrored to audit trail");
        Ok(())
    }

    /// Evict idempotency entries older than `ttl` and not still pending/running.
    /// Call this periodically to bound the idempotency map.
    pub async fn cleanup_idempotency(&self) {
        const IDEMPOTENCY_TTL_SECS: u64 = 3600; // 1 hour
        let now = std::time::Instant::now();

        // Inspect and evict under the same locks as enqueue. A stale snapshot
        // must never remove a replacement task's deduplication mapping.
        let mut tasks = self.tasks.lock().await;
        let mut idempotency = self.idempotency.lock().await;
        let mut ts = self.idempotency_ts.lock().await;
        let stale_keys: Vec<String> = ts
            .iter()
            .filter(|(_, inserted)| now.duration_since(**inserted).as_secs() > IDEMPOTENCY_TTL_SECS)
            .filter(|(key, _)| {
                idempotency
                    .get(*key)
                    .and_then(|id| tasks.get(id))
                    .is_none_or(|task| {
                        matches!(task.state, TaskState::Completed | TaskState::Cancelled)
                    })
            })
            .map(|(key, _)| key.clone())
            .collect();
        for key in stale_keys {
            if let Some(task_id) = idempotency.remove(&key) {
                tasks.remove(&task_id);
            }
            ts.remove(&key);
        }
    }

    /// Get a task's stored request for replay (used by resubscribe).
    pub async fn get_task_request(&self, task_id: &str) -> Option<VerifyRequest> {
        let audit_request_key = format!("{}:request", a2a_protocol::audit_task_key(task_id));
        match self
            .store
            .get(&audit_request_key)
            .await
            .and_then(|v| serde_json::from_value::<VerifyRequest>(v).ok())
        {
            Some(req) => Some(req),
            None => {
                warn!(task_id, "Request not found in audit trail");
                None
            }
        }
    }

    // ── Progress streaming (SSE) ────────────────────────────────────────────

    /// Push a progress event for a task. Buffers it for resubscribe and
    /// broadcasts to SSE subscribers.
    pub async fn push_progress_event(
        &self,
        task_id: &str,
        event: VerifyProgressEvent,
    ) -> Result<BufferedEvent> {
        // Buffer the event
        let mut buffers = self.event_buffers.lock().await;
        let buffer = buffers.entry(task_id.to_string()).or_default();
        let buffered = buffer.push(event);

        // Broadcast to SSE subscribers
        if let Some(tx) = self.broadcast_senders.read().await.get(task_id) {
            let _ = tx.send(buffered.clone());
        }

        debug!(
            task_id,
            seq = buffered.seq,
            "Progress event buffered and broadcast"
        );
        Ok(buffered)
    }

    /// Get a broadcast receiver for real-time SSE streaming of a task's
    /// progress events. Creates a new broadcast channel if one does not
    /// exist for the task.
    pub async fn subscribe_to_task(&self, task_id: &str) -> broadcast::Receiver<BufferedEvent> {
        let senders = self.broadcast_senders.read().await;
        if let Some(tx) = senders.get(task_id) {
            return tx.subscribe();
        }
        drop(senders);

        // Create new broadcast channel
        let mut senders = self.broadcast_senders.write().await;
        let (tx, rx) = broadcast::channel(256);
        senders.insert(task_id.to_string(), tx);
        rx
    }

    /// Replay buffered events for a task since a given sequence number.
    /// Returns empty vec if the task has no buffer.
    pub async fn replay_events_since(&self, task_id: &str, last_seq: u64) -> Vec<BufferedEvent> {
        let buffers = self.event_buffers.lock().await;
        match buffers.get(task_id) {
            Some(buffer) => buffer.events_since(last_seq),
            None => vec![],
        }
    }

    /// Get the current event buffer sequence number (0 if no events).
    pub async fn current_event_seq(&self, task_id: &str) -> u64 {
        let buffers = self.event_buffers.lock().await;
        buffers.get(task_id).map(|b| b.current_seq()).unwrap_or(0)
    }

    // ── Cancel ──────────────────────────────────────────────────────────────

    /// Set the cancel flag for a task and transition it to Cancelled state
    /// with a synthetic result, mirroring to Redis. The task stays Cancelled
    /// (not Completed) so the executor's poller can observe "cancelled" and
    /// kill the child process before the executor submits its own result.
    pub async fn cancel_task(&self, task_id: &str, result: VerifyResult) -> Result<()> {
        // Set the cancel flag
        let mut tokens = self.cancel_tokens.lock().await;
        match tokens.get(task_id) {
            Some(flag) => {
                flag.store(true, Ordering::SeqCst);
            }
            None => {
                tokens.insert(task_id.to_string(), Arc::new(AtomicBool::new(true)));
            }
        }
        drop(tokens);

        // Transition task state to Cancelled and store result
        let mut tasks = self.tasks.lock().await;
        if let Some(entry) = tasks.get_mut(task_id) {
            entry.state = TaskState::Cancelled;
            entry.result = Some(result.clone());
        }
        drop(tasks);

        self.mirror_result(&result).await?;
        debug!(task_id, "Task cancelled and result mirrored");
        Ok(())
    }

    /// Set the cancel flag for a task. Returns true if the flag was newly
    /// set, false if it was already set or the task has no cancel token.
    pub async fn mark_cancelled(&self, task_id: &str) -> bool {
        let mut tokens = self.cancel_tokens.lock().await;
        match tokens.get(task_id) {
            Some(flag) => {
                let already = flag.swap(true, Ordering::SeqCst);
                !already
            }
            None => {
                tokens.insert(task_id.to_string(), Arc::new(AtomicBool::new(true)));
                true
            }
        }
    }

    /// Create a cancel token for a task (called by executor when claiming).
    pub async fn create_cancel_token(&self, task_id: &str) -> Arc<AtomicBool> {
        let mut tokens = self.cancel_tokens.lock().await;
        tokens
            .entry(task_id.to_string())
            .or_insert_with(|| Arc::new(AtomicBool::new(false)))
            .clone()
    }

    /// Check if a task has been cancelled.
    pub async fn is_cancelled(&self, task_id: &str) -> bool {
        let tokens = self.cancel_tokens.lock().await;
        match tokens.get(task_id) {
            Some(flag) => flag.load(Ordering::SeqCst),
            None => false,
        }
    }

    /// Get a reference to the shared store.
    pub fn store(&self) -> &Arc<SharedStore> {
        &self.store
    }

    /// List all active sessions (for inspection/monitoring).
    pub async fn list_sessions(&self) -> Vec<A2ASession> {
        self.sessions.read().await.values().cloned().collect()
    }

    /// List all tracked tasks (for inspection/monitoring).
    pub async fn list_tasks(&self) -> Vec<TaskEntry> {
        self.tasks.lock().await.values().cloned().collect()
    }
}
