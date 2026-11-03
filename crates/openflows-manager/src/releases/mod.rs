//! Platform-only template release approval and promotion.
//!
//! An ordinary customer organization admin is never a platform operator.
//! Approval, promotion, and retirement require the actor to be a designated
//! platform operator (recorded in `platform_operators`, provisioned only by an
//! existing platform operator). Promotion persists the organization binding and
//! publishes the approved release; it never silently upgrades existing pins.

pub mod service;

pub use service::ReleaseApprovalService;
