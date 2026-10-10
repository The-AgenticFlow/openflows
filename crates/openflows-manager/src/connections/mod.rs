//! GitHub App connection lifecycle (WP-03).
//!
//! This module implements the complete GitHub App connection lifecycle behind
//! the Openflows Manager: connection attempts with separate user-OAuth and
//! installation-setup states, GitHub account/owner authority verification,
//! immutable installation binding, repository synchronization, signed webhook
//! ingress with durable processing, disconnect/reconnect, and testable App JWT
//! signing and installation-token exchange clients.
//!
//! Human GitHub authorization is kept strictly separate from GitHub App
//! authentication. A human OAuth token is never used as a runtime repository
//! credential, and the App private key / App JWT is never exposed to a CLI,
//! workspace, Terraform parameter, or tenant runtime.

pub mod app_jwt;
pub mod attempts;
pub mod authority;
pub mod binding;
pub mod github_app;
pub mod repository;
pub mod service;
pub mod sync;
pub mod webhooks;
pub mod worker;

pub use app_jwt::{AppJwt, AppJwtClaims, AppSigner, RealAppSigner, SignedAppJwt};
pub use attempts::AttemptService;
pub use authority::AuthorityService;
pub use binding::BindingService;
pub use github_app::{AccountType, GithubAppApi, RealGithubAppApi};
pub use repository::{ConnectionRepository, ConnectionRow, FlowType};
pub use service::{CallbackOutcome, ConnectResponse, ConnectionDto, ConnectionService};
pub use sync::SyncService;
pub use webhooks::WebhookService;
pub use worker::ConnectionWorker;
