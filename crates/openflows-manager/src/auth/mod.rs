//! Authentication module: GitHub user authorization, browser/CLI sessions,
//! device approval, and session lifecycle.
//!
//! WP-02 scope. Provider identity is always GitHub's immutable numeric user
//! id; identities are never merged by login, display name, or email. Access
//! and refresh credentials are opaque and hash-stored; only verification is
//! possible from the database.

pub mod crypto;
pub mod csrf;
pub mod device;
pub mod github;
pub mod oauth;
pub mod repository;
pub mod sessions;

pub use crypto::{EnvelopeCipher, PkcePair, Secret};
pub use github::{GithubAuth, GithubUser};
