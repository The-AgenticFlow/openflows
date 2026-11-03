//! Coder adapter layer.
//!
//! Provides a testable [`CoderAdapter`] trait and a real implementation backed
//! by `coder-client`. Every hosted operation is explicitly scoped to a Coder
//! organization id and verifies the returned resource's scope; the adapter
//! never falls back to the default organization or global administrator as the
//! workspace owner. A deterministic in-memory [`MockCoderAdapter`] backs the
//! offline tests.

pub mod adapter;
pub mod mock;
pub mod types;

pub use adapter::{CoderAdapter, RealCoderAdapter};
pub use mock::{MockCoderAdapter, MockCoderConfig};
pub use types::OrgId;
pub use types::{CoderCapabilities, PublishOutcome, PublishedTemplate, TemplatePublicationResult};
