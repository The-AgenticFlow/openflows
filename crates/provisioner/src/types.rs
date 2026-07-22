//! Shared provisioning types (Coder-only redesign).
//!
//! Most of the old pair-harness types (PairConfig, ForgeSentinelPair, etc.)
//! were local-mode-specific and have been removed. Phase 4/5 will add new
//! types as needed. This file keeps only types still referenced by
//! surviving code.

use serde::{Deserialize, Serialize};

/// Re-export CliBackend from the config crate — single source of truth.
pub use config::registry::CliBackend;

/// Files changed — can be either a count (integer) or a list of paths.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(untagged)]
pub enum FilesChanged {
    #[default]
    Unknown,
    Count(u64),
    List(Vec<String>),
}

impl FilesChanged {
    pub fn is_empty(&self) -> bool {
        match self {
            FilesChanged::Unknown => true,
            FilesChanged::Count(c) => *c == 0,
            FilesChanged::List(v) => v.is_empty(),
        }
    }
}
