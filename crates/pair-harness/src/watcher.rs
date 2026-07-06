// crates/pair-harness/src/watcher.rs
//! Filesystem watcher for event-driven harness.
//!
//! Uses notify crate for cross-platform inotify/FSEvents support.

use crate::types::FsEvent;
use anyhow::{Context, Result};
use notify::{Config, Event, EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use std::collections::HashMap;
use std::path::Path;
use std::sync::mpsc::{channel, Receiver, Sender};
use std::time::{Duration, Instant};
use tracing::{debug, error, info, warn};

const DEBOUNCE_MS: u64 = 500;
const POLL_INTERVAL_MS: u64 = 100; // low-latency fallback polling

struct EventDebouncer {
    last_seen: HashMap<String, Instant>,
}

impl EventDebouncer {
    fn new() -> Self {
        Self {
            last_seen: HashMap::new(),
        }
    }

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
}

/// Watches the shared directory for file changes.
pub struct SharedDirWatcher {
    /// The underlying notify watcher (must be kept alive)
    _watcher: RecommendedWatcher,
    /// Receiver for filesystem events
    receiver: Receiver<FsEvent>,
    /// Last seen timestamps for debouncing
    debouncer: EventDebouncer,
}

impl SharedDirWatcher {
    /// Create a new watcher for the shared directory.
    pub fn new(shared_dir: &Path) -> Result<Self> {
        let (tx, rx) = channel::<FsEvent>();

        let watcher = Self::create_watcher(tx.clone(), shared_dir)?;

        Ok(Self {
            _watcher: watcher,
            receiver: rx,
            debouncer: EventDebouncer::new(),
        })
    }

    /// Create and configure the notify watcher.
    fn create_watcher(tx: Sender<FsEvent>, shared_dir: &Path) -> Result<RecommendedWatcher> {
        // Create a watcher with a callback
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
            .configure(
                Config::default().with_poll_interval(Duration::from_millis(POLL_INTERVAL_MS)),
            )
            .context("Failed to configure watcher")?;

        // Watch the shared directory (non-recursive since we only care about top-level files)
        match watcher.watch(shared_dir, RecursiveMode::NonRecursive) {
            Ok(()) => {
                debug!(path = %shared_dir.display(), "Started watching shared directory");
                Ok(watcher)
            }
            Err(e) => {
                // Handle inotify exhaustion gracefully
                match Self::handle_watch_error(&e, shared_dir) {
                    Ok(()) => {
                        warn!("Falling back to directory polling. Performance may be slightly degraded but operation will continue.");
                        Ok(watcher)
                    }
                    Err(err) => Err(err),
                }
            }
        }
    }

    /// Handle errors from starting the directory watcher.
    fn handle_watch_error(err: &notify::Error, shared_dir: &Path) -> Result<()> {
        let err_str = err.to_string().to_lowercase();

        // Check for inotify watch limit errors
        if err_str.contains("no space left on device")
            || err_str.contains("inotify")
            || err_str.contains("too many open files")
            || Self::is_max_files_watch_error(err, &err_str)
        {
            warn!(
                path = %shared_dir.display(),
                "Filesystem watch limit reached. Falling back to polling mode."
            );
            info!("To fix this permanently, run: echo fs.inotify.max_user_watches=524288 | sudo tee -a /etc/sysctl.conf && sudo sysctl -p");
            Ok(())
        } else {
            // For other errors, propagate the error
            Err(anyhow::anyhow!(
                "Failed to start watching shared directory: {} ({})",
                shared_dir.display(),
                err
            ))
        }
    }

    /// Check if the error is related to max files watch limit
    fn is_max_files_watch_error(_err: &notify::Error, err_str: &str) -> bool {
        err_str.contains("max") && err_str.contains("watch")
            || err_str.contains("os file watch limit")
            || err_str.contains("watch limit")
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
        self.debouncer.should_emit(event)
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
    use tempfile::tempdir;

    fn event_for(path: &std::path::Path) -> Event {
        Event {
            kind: EventKind::Modify(notify::event::ModifyKind::Data(
                notify::event::DataChange::Any,
            )),
            paths: vec![path.to_path_buf()],
            attrs: Default::default(),
        }
    }

    #[test]
    fn test_classify_plan_event() {
        let dir = tempdir().unwrap();
        let plan_path = dir.path().join("PLAN.md");
        let event = event_for(&plan_path);
        assert!(matches!(
            SharedDirWatcher::classify_event(&event),
            Some(FsEvent::PlanWritten)
        ));
    }

    #[test]
    fn test_classify_segment_eval_event() {
        let dir = tempdir().unwrap();
        let eval_path = dir.path().join("segment-3-eval.md");
        let event = event_for(&eval_path);
        assert!(matches!(
            SharedDirWatcher::classify_event(&event),
            Some(FsEvent::SegmentEvalWritten(3))
        ));
    }

    #[test]
    fn test_debounce_duplicates() {
        let mut debouncer = EventDebouncer::new();
        let event = FsEvent::PlanWritten;
        assert!(debouncer.should_emit(&event));
        assert!(!debouncer.should_emit(&event));
    }
}
