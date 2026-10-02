//! openflows-harness library — typed SharedStore access for Coder Agent worker workspaces.
//!
//! This library exposes the `HarnessStore` struct which provides type-safe Redis operations
//! for managing OpenFlows state, including gate approvals for phase transitions.

pub mod a2a_client;
pub mod executor;
pub mod sandbox;
pub mod store;

pub use a2a_client::A2AClient;
pub use executor::execute_verify_task;
pub use store::{
    DispatchPayload, GateApproval, HandoffPayload, HarnessStore, MergePayload, PrInfo,
    ReviewPayload,
};

// Type alias for convenience
pub type Harness = HarnessStore;
