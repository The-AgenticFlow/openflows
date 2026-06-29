// crates/pair-harness/src/watchdog.rs
//! Watchdog for detecting stalled pairs.
//!
//! In local mode, monitors WORKLOG.md file mtime.
//! In Coder mode, stall detection is delegated to the SharedStore-based
//! event watcher — the watchdog simply treats the pair as always active.

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};
use tracing::{debug, warn};

/// Watchdog mode determines how stall detection is performed.
#[derive(Debug, Clone)]
pub enum WatchdogMode {
    /// Monitor local filesystem WORKLOG.md mtime
    Local { worklog_path: PathBuf },
    /// In Coder mode, WORKLOG.md is in the remote workspace.
    /// Stall detection is delegated to the SharedStore-based event watcher.
    Coder,
}

/// Watchdog for detecting stalled pairs.
pub struct Watchdog {
    mode: WatchdogMode,
    timeout: Duration,
    last_update: Option<DateTime<Utc>>,
    last_check: Instant,
}

impl Watchdog {
    /// Create a new watchdog for local filesystem monitoring.
    pub fn new(shared_dir: PathBuf, timeout_secs: u64) -> Self {
        Self {
            mode: WatchdogMode::Local {
                worklog_path: shared_dir.join("WORKLOG.md"),
            },
            timeout: Duration::from_secs(timeout_secs),
            last_update: None,
            last_check: Instant::now(),
        }
    }

    /// Create a new watchdog for Coder mode. In Coder mode, WORKLOG.md is
    /// written inside the remote workspace so local mtime checks are ineffective.
    /// Stall detection is delegated to the SharedStore-based event watcher.
    pub fn new_coder(timeout_secs: u64) -> Self {
        Self {
            mode: WatchdogMode::Coder,
            timeout: Duration::from_secs(timeout_secs),
            last_update: Some(Utc::now()),
            last_check: Instant::now(),
        }
    }

    /// Check if the pair is stalled (no WORKLOG update for too long).
    pub fn check_stalled(&mut self) -> Result<WatchdogStatus> {
        self.refresh_last_update()?;

        let now = Utc::now();
        let elapsed = self
            .last_update
            .map(|last| (now - last).num_seconds() as u64)
            .unwrap_or(0);

        if elapsed > self.timeout.as_secs() {
            warn!(
                elapsed_secs = elapsed,
                timeout_secs = self.timeout.as_secs(),
                "Pair appears stalled"
            );
            return Ok(WatchdogStatus::Stalled {
                last_update: self.last_update,
                elapsed: Duration::from_secs(elapsed),
            });
        }

        let warning_threshold = self.timeout.as_secs() / 2;
        if elapsed > warning_threshold {
            debug!(
                elapsed_secs = elapsed,
                warning_threshold_secs = warning_threshold,
                "Pair approaching stall threshold"
            );
            return Ok(WatchdogStatus::Warning {
                last_update: self.last_update,
                elapsed: Duration::from_secs(elapsed),
            });
        }

        Ok(WatchdogStatus::Active {
            last_update: self.last_update,
        })
    }

    /// Refresh our knowledge of the last update time.
    fn refresh_last_update(&mut self) -> Result<()> {
        match &self.mode {
            WatchdogMode::Local { worklog_path } => {
                if !worklog_path.exists() {
                    return Ok(());
                }

                let metadata =
                    fs::metadata(worklog_path).context("Failed to read WORKLOG.md metadata")?;

                let modified: std::time::SystemTime = metadata
                    .modified()
                    .context("Failed to get WORKLOG.md modification time")?;

                let modified_datetime: DateTime<Utc> = modified.into();

                if self
                    .last_update
                    .map(|l| modified_datetime > l)
                    .unwrap_or(true)
                {
                    self.last_update = Some(modified_datetime);
                    debug!(
                        last_update = %modified_datetime.to_rfc3339(),
                        "Updated last known WORKLOG modification time"
                    );
                }
            }
            WatchdogMode::Coder => {
                // In Coder mode, WORKLOG.md is in the remote workspace.
                // Stall detection is delegated to the SharedStore event watcher.
                // Keep last_update as-is (set on init/reset).
            }
        }

        Ok(())
    }

    /// Get the last known update time.
    pub fn last_update(&self) -> Option<DateTime<Utc>> {
        self.last_update
    }

    /// Reset the watchdog (call when activity is detected).
    pub fn reset(&mut self) {
        self.last_update = Some(Utc::now());
        self.last_check = Instant::now();
        debug!("Watchdog reset");
    }

    /// Check for segment loop (same segment evaluated too many times).
    /// Only effective in Local mode; Coder mode delegates to SharedStore watchers.
    pub fn check_segment_loop(
        &self,
        shared_dir: &PathBuf,
        segment: u32,
        max_iterations: u32,
    ) -> Result<bool> {
        match &self.mode {
            WatchdogMode::Local { .. } => {
                let mut eval_count = 0;

                for entry in fs::read_dir(shared_dir).context("Failed to read shared directory")? {
                    let entry = entry?;
                    let filename = entry.file_name().to_string_lossy().to_string();

                    if filename == format!("segment-{}-eval.md", segment) {
                        let content = fs::read_to_string(entry.path())?;
                        if content.contains("CHANGES_REQUESTED") {
                            eval_count += 1;
                        }
                    }
                }

                if eval_count > max_iterations {
                    warn!(
                        segment = segment,
                        iterations = eval_count,
                        max_iterations = max_iterations,
                        "Segment loop detected"
                    );
                    return Ok(true);
                }
            }
            WatchdogMode::Coder => {
                debug!(
                    "Segment loop check skipped in Coder mode — handled by SharedStore watcher"
                );
            }
        }

        Ok(false)
    }
}

/// Status returned by the watchdog.
#[derive(Debug, Clone)]
pub enum WatchdogStatus {
    /// Pair is active and making progress
    Active { last_update: Option<DateTime<Utc>> },
    /// Pair is approaching stall threshold
    Warning {
        last_update: Option<DateTime<Utc>>,
        elapsed: Duration,
    },
    /// Pair is stalled (no updates for too long)
    Stalled {
        last_update: Option<DateTime<Utc>>,
        elapsed: Duration,
    },
}

impl WatchdogStatus {
    /// Check if the status indicates stalled.
    pub fn is_stalled(&self) -> bool {
        matches!(self, WatchdogStatus::Stalled { .. })
    }

    /// Check if the status indicates a warning.
    pub fn is_warning(&self) -> bool {
        matches!(self, WatchdogStatus::Warning { .. })
    }

    /// Get the elapsed time (if available).
    pub fn elapsed(&self) -> Option<Duration> {
        match self {
            WatchdogStatus::Stalled { elapsed, .. } => Some(*elapsed),
            WatchdogStatus::Warning { elapsed, .. } => Some(*elapsed),
            WatchdogStatus::Active { .. } => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;
    use tempfile::tempdir;

    #[test]
    fn test_watchdog_active() {
        let dir = tempdir().unwrap();
        let shared = dir.path().to_path_buf();

        // Create WORKLOG.md
        fs::write(shared.join("WORKLOG.md"), "# Worklog\n").unwrap();

        let mut watchdog = Watchdog::new(shared, 1200); // 20 minutes

        let status = watchdog.check_stalled().unwrap();
        assert!(!status.is_stalled());
    }

    #[test]
    fn test_watchdog_stalled() {
        let dir = tempdir().unwrap();
        let shared = dir.path().to_path_buf();

        let mut watchdog = Watchdog::new(shared.clone(), 1); // 1 second timeout

        // No WORKLOG.md exists - should not be stalled yet
        let status = watchdog.check_stalled().unwrap();
        assert!(!status.is_stalled());

        // Create WORKLOG.md
        fs::write(shared.join("WORKLOG.md"), "# Worklog\n").unwrap();

        // Wait for timeout (double the timeout to be safe)
        thread::sleep(Duration::from_millis(2100));

        // Now check - should be stalled
        let status = watchdog.check_stalled().unwrap();
        assert!(status.is_stalled());
    }

    #[test]
    fn test_watchdog_reset() {
        let dir = tempdir().unwrap();
        let shared = dir.path().to_path_buf();

        let mut watchdog = Watchdog::new(shared, 1);

        // Reset the watchdog
        watchdog.reset();

        // Should be active
        let status = watchdog.check_stalled().unwrap();
        assert!(!status.is_stalled());
    }

    #[test]
    fn test_watchdog_stale_worklog_reset() {
        let dir = tempdir().unwrap();
        let shared = dir.path().to_path_buf();

        // Simulate a previous lifecycle: create WORKLOG.md and let it age
        let mut watchdog = Watchdog::new(shared.clone(), 1200);
        fs::write(shared.join("WORKLOG.md"), "# Worklog from previous run\n").unwrap();

        // Wait briefly so the file has a non-zero age
        thread::sleep(Duration::from_millis(100));

        // Without reset, the watchdog would calculate elapsed from the old mtime.
        // But we call reset() as pair re-provisioning does.
        watchdog.reset();

        // Should be active — reset guarantees the watchdog treats "now" as last update
        let status = watchdog.check_stalled().unwrap();
        assert!(!status.is_stalled());
    }

    #[test]
    fn test_watchdog_coder_mode_always_active() {
        let mut watchdog = Watchdog::new_coder(1); // 1 second timeout

        // Coder mode always treats last_update as current
        let status = watchdog.check_stalled().unwrap();
        assert!(!status.is_stalled());
    }
}
