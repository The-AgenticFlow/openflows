//! Library entry point for the OpenFlows Manager HTTP service.

pub mod audit;
pub mod auth;
pub mod config;
pub mod db;
pub mod dto;
pub mod error;
pub mod id;
pub mod idempotency;
pub mod operations;
pub mod organizations;
pub mod outbox;
pub mod pages;
pub mod pagination;
pub mod rate_limit;
pub mod repositories;
pub mod routes;
pub mod secrets;
pub mod server;

pub use config::{ManagerConfig, Mode};
pub use db::Db;
pub use error::{ApiError, ManagerError};
pub use id::*;
pub use secrets::{SecretProvider, SecretRef};
