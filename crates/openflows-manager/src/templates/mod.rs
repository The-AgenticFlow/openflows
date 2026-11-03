//! Template release packaging, validation, persistence, and publication.
//!
//! A release is a set of five deterministic role archives plus an immutable,
//! versioned manifest whose digest is the release identity (never a mutable
//! `latest` reference). Packaging normalizes timestamps, ownership, and gzip
//! metadata so identical inputs produce identical artifacts. Archives and
//! manifests are validated before use, and unsafe archive entries (traversal,
//! links, special files) are rejected. The old agent `registry.json` is never
//! shipped or passed as a template parameter here.

pub mod packaging;
pub mod repository;
pub mod service;

pub use packaging::{build_deterministic_archive, Role, RoleManifestEntry, TemplateManifest};
pub use repository::ReleaseRepository;
pub use service::ReleaseService;
