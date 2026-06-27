// crates/pair-harness/src/lib.rs
//! Event-driven harness for FORGE-SENTINEL pair lifecycle management.
//!
//! This crate implements the v3 architecture where:
//! - SENTINEL is ephemeral (spawned fresh per segment evaluation)
//! - The harness uses inotify/FSEvents for zero-polling event detection
//! - File locking uses flock for atomic acquisition
//! - Both FORGE and SENTINEL run with auto-mode permissions

pub mod isolation;
pub mod mcp_config;
pub mod pair;
pub mod pair_state;
pub mod process;
pub mod provision;
pub mod reset;
pub mod responses_proxy;
pub mod transport;
pub mod types;
pub mod watchdog;
pub mod watcher;
pub mod workspace;
pub mod worktree;

pub use isolation::FileLockManager;
pub use mcp_config::McpConfigGenerator;
pub use pair::ForgeSentinelPair;
pub use pair_state::{
    CoderWatcherAdapter, FilesystemPairState, FilesystemWatcher, PairArtifact, PairStateStore,
    PairStateWatcher, PairWatcher, SharedStorePairState, SharedStoreWatcher, WatcherAdapter,
};
pub use process::{ProcessManager, SentinelMode};
pub use provision::Provisioner;
pub use reset::ResetManager;
pub use responses_proxy::start_responses_proxy;
pub use transport::{CommandOutput, DirEntry, LocalTransport, WorkspaceTransport};
pub use types::{
    CliBackend, ErrorHistory, ErrorHistoryEntry, FsEvent, PairConfig, PairOutcome, Ticket,
    VerificationResult, VerificationState,
};
pub use watchdog::Watchdog;
pub use watcher::SharedDirWatcher;
pub use workspace::WorkspaceManager;
pub use worktree::{SetupWarning, WorktreeManager, WorktreeSetupResult};
