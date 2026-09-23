// crates/agent-vessel/src/lib.rs
//
// VESSEL Agent — DevOps Specialist and Merge Gatekeeper.
//
// The only agent authorized to perform destructive/irreversible actions
// (merging PRs, deploying). Ensures CI passes before merging and emits
// ticket_merged events critical for dependency resolution.

pub mod ci_poller;
pub mod conflict_resolver;
pub mod merger;
pub mod node;
pub mod notifier;
pub mod pr_monitor;
pub mod types;

pub use ci_poller::CiPoller;
pub use conflict_resolver::{ConflictResolution, ConflictResolver};
pub use merger::PrMerger;
pub use node::VesselNode;
pub use notifier::VesselNotifier;
pub use pr_monitor::{
    build_directive, classify, classify_from_parts, collect_rework, PrMonitorState, ReworkDirective,
};
pub use types::{CiReadiness, VesselConfig, VesselOutcome};
