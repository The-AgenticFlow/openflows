// crates/pair-harness/src/watcher.rs
//! Filesystem watcher for event-driven harness.
//!
//! Uses notify crate for cross-platform inotify/FSEvents support.
//! Falls back to polling when inotify watch limits are exhausted.

use crate::types::FsEvent;
use anyhow::{Context, Result};
use notify::{Config, Event, EventKind, PollWatcher, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use tracing::{debug, error, warn};

const DEBOUNCE_MS: u64 = 500;
const POLL_INTERVAL_MS: u64 = 500;

#[allow(dead_code)]
enum WatcherInner {
    Recommended(RecommendedWatcher),
    Poll(PollWatcher),
}

/// Watches the shared directory for file changes.
pub struct SharedDirWatcher {
    /// The underlying notify watcher (must be kept alive)
    _watcher: WatcherInner,
    /// Receiver for filesystem events
    receiver: Receiver<FsEvent>,
    /// Last seen timestamps for debouncing
    last_seen: HashMap<String, Instant>,
}

impl SharedDirWatcher {
    /// Create a new watcher for the shared directory.
    pub fn new(shared_dir: &Path) -> Result<Self> {
        let (tx, rx) = channel::<FsEvent>();

        let watcher = Self::create_watcher(tx.clone(), shared_dir)?;

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
            last_seen: HashMap::new(),
        })
    }

    /// Create and configure the notify watcher, falling back to PollWatcher
    /// if the RecommendedWatcher (inotify/FSEvents) cannot be started due to
    /// OS resource limits (e.g. inotify max_user_watches exhausted on Linux).
    fn create_watcher(tx: Sender<FsEvent>, shared_dir: &Path) -> Result<WatcherInner> {
        match Self::try_recommended_watcher(tx.clone(), shared_dir) {
            Ok(w) => Ok(w),
            Err(e) => {
                warn!(error = %e, "RecommendedWatcher failed, falling back to PollWatcher");
                Self::create_poll_watcher(tx, shared_dir)
            }
        }
    }

    fn try_recommended_watcher(tx: Sender<FsEvent>, shared_dir: &Path) -> Result<WatcherInner> {
        let mut watcher = notify::recommended_watcher(move |res: notify::Result<Event>| {
            match res {
                Ok(event) => {
                    if let Some(fs_event) = Self::classify_event(&event) {
                        debug!(event = ?fs_event, paths = ?event.paths, "Filesystem event detected");
                        let _ = tx.send(fs_event);
                    }
                }
                Err(e) => {
                    error!(error = %e, "Watch error");
                }
            }
        }).context("Failed to create filesystem watcher")?;

        // Configure for low latency
        watcher
            .configure(Config::default().with_poll_interval(Duration::from_millis(100)))
            .context("Failed to configure watcher")?;

        // Watch the shared directory (non-recursive since we only care about top-level files)
        watcher
            .watch(shared_dir, RecursiveMode::NonRecursive)
            .context("Failed to start watching shared directory")?;

        debug!(path = %shared_dir.display(), "Started watching shared directory (inotify)");
        Ok(WatcherInner::Recommended(watcher))
    }

    fn create_poll_watcher(tx: Sender<FsEvent>, shared_dir: &Path) -> Result<WatcherInner> {
        let config = Config::default().with_poll_interval(Duration::from_millis(POLL_INTERVAL_MS));

        let mut watcher = PollWatcher::new(
            move |res: notify::Result<Event>| {
                match res {
                    Ok(event) => {
                        if let Some(fs_event) = Self::classify_event(&event) {
                            debug!(event = ?fs_event, paths = ?event.paths, "Filesystem event detected (poll)");
                            let _ = tx.send(fs_event);
                        }
                    }
                    Err(e) => {
                        error!(error = %e, "Poll watch error");
                    }
                }
            },
            config,
        ).context("Failed to create poll filesystem watcher")?;

        watcher
            .watch(shared_dir, RecursiveMode::NonRecursive)
            .context("Failed to start polling shared directory")?;

        debug!(path = %shared_dir.display(), "Started watching shared directory (polling fallback)");
        Ok(WatcherInner::Poll(watcher))
    }

    /// Classify a filesystem event into our FsEvent type.
    fn classify_event(event: &Event) -> Option<FsEvent> {
        // Only care about create and modify events
        match event.kind {
            EventKind::Create(_) | EventKind::Modify(_) | EventKind::Access(_) => {}
            _ => return None,
        }

        // Check each path in the event
        for path in &event.paths {
            let filename = path.file_name()?.to_str()?;

            let fs_event = match filename {
                "PLAN.md" => Some(FsEvent::PlanWritten),
                "CONTRACT.md" => Some(FsEvent::ContractWritten),
                "WORKLOG.md" => Some(FsEvent::WorklogUpdated),
                "final-review.md" => Some(FsEvent::FinalReviewWritten),
                "STATUS.json" => Some(FsEvent::StatusJsonWritten),
                "HANDOFF.md" => Some(FsEvent::HandoffWritten),
                s if s.starts_with("segment-") && s.ends_with("-eval.md") => {
                    // Extract segment number from "segment-N-eval.md"
                    let n = s
                        .strip_prefix("segment-")?
                        .strip_suffix("-eval.md")?
                        .parse::<u32>()
                        .ok()?;
                    Some(FsEvent::SegmentEvalWritten(n))
                }
                _ => None,
            };

            if fs_event.is_some() {
                return fs_event;
            }
        }

        None
    }

    /// Try to receive an event without blocking.
    pub fn try_recv(&mut self) -> Option<FsEvent> {
        loop {
            match self.receiver.try_recv() {
                Ok(event) => {
                    if self.should_emit(&event) {
                        return Some(event);
                    }
                    debug!(event = ?event, "Debounced duplicate event");
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => return None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => return None,
            }
        }
    }

    /// Receive an event with a timeout.
    pub fn recv_timeout(&mut self, timeout: Duration) -> Option<FsEvent> {
        let start = Instant::now();
        loop {
            let remaining = timeout.saturating_sub(start.elapsed());
            if remaining.is_zero() {
                return None;
            }

            match self.receiver.recv_timeout(remaining) {
                Ok(event) => {
                    if self.should_emit(&event) {
                        return Some(event);
                    }
                    debug!(event = ?event, "Debounced duplicate event");
                }
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => return None,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => return None,
            }
        }
    }

    /// Check if an event should be emitted (debounce logic).
    fn should_emit(&mut self, event: &FsEvent) -> bool {
        let key = format!("{:?}", event);
        let now = Instant::now();

        if let Some(last) = self.last_seen.get(&key) {
            if now.duration_since(*last) < Duration::from_millis(DEBOUNCE_MS) {
                return false;
            }
        }

        self.last_seen.insert(key, now);
        true
    }

    /// Get a reference to the underlying receiver for use in async contexts.
    pub fn receiver(&self) -> &Receiver<FsEvent> {
        &self.receiver
    }
}

/// Async wrapper for the watcher that integrates with tokio.
pub struct AsyncWatcher {
    /// The underlying watcher
    watcher: SharedDirWatcher,
}

impl AsyncWatcher {
    /// Create a new async watcher.
    pub fn new(shared_dir: &Path) -> Result<Self> {
        let watcher = SharedDirWatcher::new(shared_dir)?;
        Ok(Self { watcher })
    }

    /// Receive events as a stream.
    pub fn recv(&self) -> Option<FsEvent> {
        // This is a blocking call, but that's okay for our event-driven architecture
        self.watcher.receiver().recv().ok()
    }

    /// Try to receive without blocking.
    pub fn try_recv(&mut self) -> Option<FsEvent> {
        self.watcher.try_recv()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn test_classify_plan_event() {
        let dir = tempdir().unwrap();
        let shared = dir.path();

        let mut watcher = SharedDirWatcher::new(shared).unwrap();

        // Write PLAN.md
        let plan_path = shared.join("PLAN.md");
        fs::write(&plan_path, "# Plan\n").unwrap();

        // Give the watcher time to detect
        std::thread::sleep(Duration::from_millis(200));

        let event = watcher.try_recv();
        assert!(matches!(event, Some(FsEvent::PlanWritten)));
    }

    #[test]
    fn test_classify_segment_eval_event() {
        let dir = tempdir().unwrap();
        let shared = dir.path();

        let mut watcher = SharedDirWatcher::new(shared).unwrap();

        // Write segment-3-eval.md
        let eval_path = shared.join("segment-3-eval.md");
        fs::write(&eval_path, "# Eval\n").unwrap();

        // Give the watcher time to detect
        std::thread::sleep(Duration::from_millis(200));

        let event = watcher.try_recv();
        assert!(matches!(event, Some(FsEvent::SegmentEvalWritten(3))));
    }

    #[test]
    fn test_debounce_duplicates() {
        let dir = tempdir().unwrap();
        let shared = dir.path();

        let mut watcher = SharedDirWatcher::new(shared).unwrap();

        // Write PLAN.md
        let plan_path = shared.join("PLAN.md");
        fs::write(&plan_path, "# Plan\n").unwrap();

        // Give the watcher time to detect (might get multiple events)
        std::thread::sleep(Duration::from_millis(200));

        // Should get one event
        let event1 = watcher.try_recv();
        assert!(matches!(event1, Some(FsEvent::PlanWritten)));

        // Immediately check again - should be debounced
        let event2 = watcher.try_recv();
        assert!(event2.is_none());
    }
}
