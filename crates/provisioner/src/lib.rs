//! pair-harness — Coder workspace transport and provisioning.
//!
//! After the Coder-only redesign, this crate contains only:
//! - `transport`: `WorkspaceTransport` trait + `CoderTransport` implementation
//! - `provision`: `Provisioner` for materializing skills/MCP/standards into workspaces
//! - `types`: shared provisioning types

pub mod provision;
pub mod transport;
pub mod types;

pub use provision::Provisioner;
pub use transport::{CommandOutput, DirEntry, CoderTransport, WorkspaceTransport};
