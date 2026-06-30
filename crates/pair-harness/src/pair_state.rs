// crates/pair-harness/src/pair_state.rs
//! Dual-backend pair state storage for FORGE-SENTINEL coordination.
//!
//! In local mode, pair artifacts (STATUS.json, PLAN.md, etc.) are stored on
//! the filesystem in `.pair-shared/`. In Coder mode, they're stored in
//! SharedStore (Redis) so cross-workspace coordination works.
//!
//! The `PairStateStore` trait abstracts over both backends. `PairStateWatcher`
//! abstracts over filesystem inotify and SharedStore polling.

use anyhow::Result;
use async_trait::async_trait;
use std::path::{Path, PathBuf};
use std::time::Duration;
use tracing::debug;

use crate::types::FsEvent;

/// Artifact types that FORGE and SENTINEL exchange.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PairArtifact {
    Status,
    Worklog,
    Ticket,
    Task,
    Plan,
    Contract,
    Handoff,
    SegmentEval(usize),
    FinalReview,
    ErrorFeedback,
    ConflictResolution,
    CiFix,
}

impl PairArtifact {
    pub fn filename(&self) -> String {
        match self {
            PairArtifact::Status => "STATUS.json".to_string(),
            PairArtifact::Worklog => "WORKLOG.md".to_string(),
            PairArtifact::Ticket => "TICKET.md".to_string(),
            PairArtifact::Task => "TASK.md".to_string(),
            PairArtifact::Plan => "PLAN.md".to_string(),
            PairArtifact::Contract => "CONTRACT.md".to_string(),
            PairArtifact::Handoff => "HANDOFF.md".to_string(),
            PairArtifact::SegmentEval(n) => format!("segment-{}-eval.md", n),
            PairArtifact::FinalReview => "final-review.md".to_string(),
            PairArtifact::ErrorFeedback => "ERROR_FEEDBACK.md".to_string(),
            PairArtifact::ConflictResolution => "CONFLICT_RESOLUTION.md".to_string(),
            PairArtifact::CiFix => "CI_FIX.md".to_string(),
        }
    }

    pub fn shared_store_key(&self, pair_id: &str) -> String {
        match self {
            PairArtifact::Status => pocketflow_core::pair_keys::pair_keys::status(pair_id),
            PairArtifact::Worklog => pocketflow_core::pair_keys::pair_keys::worklog(pair_id),
            PairArtifact::Ticket => pocketflow_core::pair_keys::pair_keys::ticket(pair_id),
            PairArtifact::Task => pocketflow_core::pair_keys::pair_keys::task(pair_id),
            PairArtifact::Plan => pocketflow_core::pair_keys::pair_keys::plan(pair_id),
            PairArtifact::Contract => pocketflow_core::pair_keys::pair_keys::contract(pair_id),
            PairArtifact::Handoff => pocketflow_core::pair_keys::pair_keys::handoff(pair_id),
            PairArtifact::SegmentEval(n) => {
                pocketflow_core::pair_keys::pair_keys::segment_eval(pair_id, *n)
            }
            PairArtifact::FinalReview => {
                pocketflow_core::pair_keys::pair_keys::final_review(pair_id)
            }
            PairArtifact::ErrorFeedback => {
                pocketflow_core::pair_keys::pair_keys::error_feedback(pair_id)
            }
            PairArtifact::ConflictResolution => {
                pocketflow_core::pair_keys::pair_keys::conflict_resolution(pair_id)
            }
            PairArtifact::CiFix => pocketflow_core::pair_keys::pair_keys::ci_fix(pair_id),
        }
    }

    pub fn from_filename(filename: &str) -> Option<PairArtifact> {
        match filename {
            "STATUS.json" => Some(PairArtifact::Status),
            "WORKLOG.md" => Some(PairArtifact::Worklog),
            "TICKET.md" => Some(PairArtifact::Ticket),
            "TASK.md" => Some(PairArtifact::Task),
            "PLAN.md" => Some(PairArtifact::Plan),
            "CONTRACT.md" => Some(PairArtifact::Contract),
            "HANDOFF.md" => Some(PairArtifact::Handoff),
            "final-review.md" => Some(PairArtifact::FinalReview),
            "ERROR_FEEDBACK.md" => Some(PairArtifact::ErrorFeedback),
            "CONFLICT_RESOLUTION.md" => Some(PairArtifact::ConflictResolution),
            "CI_FIX.md" => Some(PairArtifact::CiFix),
            s if s.starts_with("segment-") && s.ends_with("-eval.md") => {
                let n = s
                    .strip_prefix("segment-")?
                    .strip_suffix("-eval.md")?
                    .parse::<usize>()
                    .ok()?;
                Some(PairArtifact::SegmentEval(n))
            }
            _ => None,
        }
    }

    pub fn to_fs_event(&self) -> Option<FsEvent> {
        match self {
            PairArtifact::Plan => Some(FsEvent::PlanWritten),
            PairArtifact::Contract => Some(FsEvent::ContractWritten),
            PairArtifact::Worklog => Some(FsEvent::WorklogUpdated),
            PairArtifact::FinalReview => Some(FsEvent::FinalReviewWritten),
            PairArtifact::Status => Some(FsEvent::StatusJsonWritten),
            PairArtifact::Handoff => Some(FsEvent::HandoffWritten),
            PairArtifact::SegmentEval(n) => Some(FsEvent::SegmentEvalWritten(*n as u32)),
            _ => None,
        }
    }
}

/// Trait for reading/writing pair artifacts. Two implementations:
/// - `FilesystemPairState`: reads/writes `.pair-shared/` directory (local mode)
/// - `SharedStorePairState`: reads/writes `SharedStore` keys (Coder mode)
#[async_trait]
pub trait PairStateStore: Send + Sync {
    async fn read_artifact(&self, pair_id: &str, artifact: PairArtifact) -> Result<Option<String>>;
    async fn write_artifact(
        &self,
        pair_id: &str,
        artifact: PairArtifact,
        content: &str,
    ) -> Result<()>;
}

/// Filesystem-based pair state store. Reads/writes to `.pair-shared/` directory.
pub struct FilesystemPairState {
    shared_dir: PathBuf,
}

impl FilesystemPairState {
    pub fn new(shared_dir: impl Into<PathBuf>) -> Self {
        Self {
            shared_dir: shared_dir.into(),
        }
    }
}

#[async_trait]
impl PairStateStore for FilesystemPairState {
    async fn read_artifact(
        &self,
        _pair_id: &str,
        artifact: PairArtifact,
    ) -> Result<Option<String>> {
        let path = self.shared_dir.join(artifact.filename());
        if path.exists() {
            let content = tokio::fs::read_to_string(&path).await?;
            Ok(Some(content))
        } else {
            Ok(None)
        }
    }

    async fn write_artifact(
        &self,
        _pair_id: &str,
        artifact: PairArtifact,
        content: &str,
    ) -> Result<()> {
        let path = self.shared_dir.join(artifact.filename());
        debug!(path = %path.display(), artifact = ?artifact, "Writing pair artifact to filesystem");
        if let Some(parent) = path.parent() {
            tokio::fs::create_dir_all(parent).await?;
        }
        tokio::fs::write(&path, content).await?;
        Ok(())
    }
}

/// SharedStore-based pair state store. Reads/writes to Redis-backed keys.
pub struct SharedStorePairState {
    store: pocketflow_core::SharedStore,
}

impl SharedStorePairState {
    pub fn new(store: pocketflow_core::SharedStore) -> Self {
        Self { store }
    }
}

#[async_trait]
impl PairStateStore for SharedStorePairState {
    async fn read_artifact(&self, pair_id: &str, artifact: PairArtifact) -> Result<Option<String>> {
        let key = artifact.shared_store_key(pair_id);
        let value = self.store.get(&key).await;
        match value {
            Some(v) => Ok(Some(v.as_str().unwrap_or_default().to_string())),
            None => Ok(None),
        }
    }

    async fn write_artifact(
        &self,
        pair_id: &str,
        artifact: PairArtifact,
        content: &str,
    ) -> Result<()> {
        let key = artifact.shared_store_key(pair_id);
        debug!(key = %key, artifact = ?artifact, "Writing pair artifact to SharedStore");
        self.store
            .set(&key, serde_json::Value::String(content.to_string()))
            .await;
        Ok(())
    }
}

/// Trait for watching pair state changes. Two implementations:
/// - `FilesystemWatcher`: current inotify-based watching on `.pair-shared/`
/// - `SharedStoreWatcher`: polls SharedStore for key changes
#[async_trait]
pub trait PairStateWatcher: Send + Sync {
    async fn wait_for_change(
        &self,
        pair_id: &str,
        artifact: PairArtifact,
        timeout: Duration,
    ) -> Result<String>;
}

/// Unified event watcher that can operate in local mode (SharedDirWatcher / inotify)
/// or Coder mode (SharedStoreWatcher / polling). Both produce `FsEvent` values
/// so the pair event loop can remain unchanged.
pub enum PairWatcher {
    Local(WatcherAdapter),
    Coder(CoderWatcherAdapter),
}

/// Adapter wrapping SharedDirWatcher into an FsEvent-producing watcher.
pub struct WatcherAdapter {
    inner: crate::watcher::SharedDirWatcher,
}

impl WatcherAdapter {
    pub fn new(shared_dir: &std::path::Path) -> Result<Self> {
        Ok(Self {
            inner: crate::watcher::SharedDirWatcher::new(shared_dir)?,
        })
    }

    pub fn try_recv(&mut self) -> Option<crate::types::FsEvent> {
        self.inner.try_recv()
    }

    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<crate::types::FsEvent> {
        self.inner.recv_timeout(timeout)
    }
}

/// Adapter wrapping SharedStoreWatcher to produce FsEvent values via background polling.
/// Spawns a background thread that polls SharedStore and sends events through a channel,
/// matching the `SharedDirWatcher` interface of `try_recv` / `recv_timeout`.
pub struct CoderWatcherAdapter {
    receiver: std::sync::mpsc::Receiver<crate::types::FsEvent>,
}

impl CoderWatcherAdapter {
    pub fn new(store: pocketflow_core::SharedStore, pair_id: &str) -> Self {
        let (tx, rx) = std::sync::mpsc::channel::<crate::types::FsEvent>();
        let pair_id = pair_id.to_string();
        let poll_interval = Duration::from_millis(200);

        let artifacts: Vec<PairArtifact> = vec![
            PairArtifact::Status,
            PairArtifact::Plan,
            PairArtifact::Contract,
            PairArtifact::Worklog,
            PairArtifact::Handoff,
            PairArtifact::FinalReview,
            PairArtifact::ErrorFeedback,
            PairArtifact::Ticket,
            PairArtifact::Task,
            PairArtifact::ConflictResolution,
            PairArtifact::CiFix,
        ];

        std::thread::spawn(move || {
            let rt =
                tokio::runtime::Runtime::new().expect("Failed to create tokio runtime for watcher");
            let mut last_values: std::collections::HashMap<String, String> =
                std::collections::HashMap::new();

            loop {
                for artifact in &artifacts {
                    let key = artifact.shared_store_key(&pair_id);
                    let value = rt.block_on(async { store.get(&key).await });

                    let content = value
                        .as_ref()
                        .map(|v| v.as_str().unwrap_or("").to_string())
                        .unwrap_or_default();
                    let changed = match last_values.get(&key) {
                        Some(last) => *last != content,
                        None => !content.is_empty(),
                    };
                    if changed {
                        last_values.insert(key, content);
                        if let Some(fs_event) = artifact.to_fs_event() {
                            if tx.send(fs_event).is_err() {
                                return;
                            }
                        }
                    }
                }

                std::thread::sleep(poll_interval);
            }
        });

        Self { receiver: rx }
    }

    pub fn try_recv(&mut self) -> Option<crate::types::FsEvent> {
        match self.receiver.try_recv() {
            Ok(event) => Some(event),
            Err(std::sync::mpsc::TryRecvError::Empty) => None,
            Err(std::sync::mpsc::TryRecvError::Disconnected) => None,
        }
    }

    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<crate::types::FsEvent> {
        self.receiver.recv_timeout(timeout).ok()
    }
}

impl PairWatcher {
    pub fn try_recv(&mut self) -> Option<crate::types::FsEvent> {
        match self {
            PairWatcher::Local(w) => w.try_recv(),
            PairWatcher::Coder(w) => w.try_recv(),
        }
    }

    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<crate::types::FsEvent> {
        match self {
            PairWatcher::Local(w) => w.recv_timeout(timeout),
            PairWatcher::Coder(w) => w.recv_timeout(timeout),
        }
    }
}

/// Filesystem-based watcher using inotify (wraps SharedDirWatcher).
pub struct FilesystemWatcher {
    shared_dir: PathBuf,
}

impl FilesystemWatcher {
    pub fn new(shared_dir: impl Into<PathBuf>) -> Self {
        Self {
            shared_dir: shared_dir.into(),
        }
    }

    pub fn shared_dir(&self) -> &Path {
        &self.shared_dir
    }
}

#[async_trait]
impl PairStateWatcher for FilesystemWatcher {
    async fn wait_for_change(
        &self,
        _pair_id: &str,
        artifact: PairArtifact,
        timeout: Duration,
    ) -> Result<String> {
        let path = self.shared_dir.join(artifact.filename());
        let start = std::time::Instant::now();
        let mut last_mtime = tokio::fs::metadata(&path)
            .await
            .ok()
            .and_then(|m| m.modified().ok());

        while start.elapsed() < timeout {
            tokio::time::sleep(Duration::from_millis(200)).await;
            let current_mtime = tokio::fs::metadata(&path)
                .await
                .ok()
                .and_then(|m| m.modified().ok());
            if current_mtime != last_mtime {
                if let Some(content) = tokio::fs::read_to_string(&path).await.ok() {
                    return Ok(content);
                }
            }
            last_mtime = current_mtime;
        }
        anyhow::bail!(
            "Timeout waiting for artifact change: {}",
            artifact.filename()
        )
    }
}

/// SharedStore-based watcher that polls for artifact changes.
pub struct SharedStoreWatcher {
    store: pocketflow_core::SharedStore,
    poll_interval: Duration,
}

impl SharedStoreWatcher {
    pub fn new(store: pocketflow_core::SharedStore) -> Self {
        Self {
            store,
            poll_interval: Duration::from_millis(200),
        }
    }

    pub fn with_poll_interval(mut self, poll_interval: Duration) -> Self {
        self.poll_interval = poll_interval;
        self
    }
}

#[async_trait]
impl PairStateWatcher for SharedStoreWatcher {
    async fn wait_for_change(
        &self,
        pair_id: &str,
        artifact: PairArtifact,
        timeout: Duration,
    ) -> Result<String> {
        let start = std::time::Instant::now();
        let mut last_value = self.store.get(&artifact.shared_store_key(pair_id)).await;

        while start.elapsed() < timeout {
            tokio::time::sleep(self.poll_interval).await;
            let current_value = self.store.get(&artifact.shared_store_key(pair_id)).await;
            if current_value != last_value {
                if let Some(value) = current_value {
                    return Ok(value.as_str().unwrap_or_default().to_string());
                }
                return Ok(String::new());
            }
            last_value = current_value;
        }

        anyhow::bail!(
            "Timeout waiting for SharedStore artifact change: {}",
            artifact.filename()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[tokio::test]
    async fn test_filesystem_pair_state_round_trip() {
        let dir = tempdir().unwrap();
        let store = FilesystemPairState::new(dir.path());

        for artifact in [
            PairArtifact::Status,
            PairArtifact::Task,
            PairArtifact::Plan,
            PairArtifact::Contract,
            PairArtifact::Worklog,
            PairArtifact::FinalReview,
            PairArtifact::SegmentEval(1),
        ] {
            store
                .write_artifact("pair-1", artifact, "test content")
                .await
                .unwrap();
            let result = store
                .read_artifact("pair-1", artifact)
                .await
                .unwrap()
                .unwrap();
            assert_eq!(result, "test content", "Failed for {:?}", artifact);
        }
    }

    #[tokio::test]
    async fn test_filesystem_read_nonexistent() {
        let dir = tempdir().unwrap();
        let store = FilesystemPairState::new(dir.path());

        let result = store
            .read_artifact("pair-1", PairArtifact::Plan)
            .await
            .unwrap();
        assert!(result.is_none());
    }

    #[test]
    fn test_artifact_filename_round_trip() {
        let artifacts = [
            PairArtifact::Status,
            PairArtifact::Worklog,
            PairArtifact::Task,
            PairArtifact::Plan,
            PairArtifact::Contract,
            PairArtifact::FinalReview,
            PairArtifact::SegmentEval(3),
        ];
        for artifact in &artifacts {
            let filename = artifact.filename();
            let parsed = PairArtifact::from_filename(&filename);
            assert_eq!(
                parsed.as_ref(),
                Some(artifact),
                "Round-trip failed for {:?}",
                artifact
            );
        }
    }

    #[test]
    fn test_shared_store_key_format() {
        let key = PairArtifact::Status.shared_store_key("forge-1");
        assert_eq!(key, "pair:forge-1:status");

        let key = PairArtifact::SegmentEval(5).shared_store_key("forge-2");
        assert_eq!(key, "pair:forge-2:segment:5:eval");
    }

    #[tokio::test]
    async fn test_shared_store_watcher_detects_change() {
        let store = pocketflow_core::SharedStore::new_in_memory();
        let watcher =
            SharedStoreWatcher::new(store.clone()).with_poll_interval(Duration::from_millis(20));
        let pair_id = "pair-1";
        let artifact = PairArtifact::Plan;

        let store2 = store.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(40)).await;
            store2
                .set(
                    &artifact.shared_store_key(pair_id),
                    serde_json::Value::String("updated".to_string()),
                )
                .await;
        });

        let content = watcher
            .wait_for_change(pair_id, artifact, Duration::from_secs(2))
            .await
            .unwrap();
        assert_eq!(content, "updated");
    }
}
