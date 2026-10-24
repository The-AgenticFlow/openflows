//! Library entry point for the OpenFlows Manager HTTP service.

pub mod audit;
pub mod auth;
pub mod coder;
pub mod config;
pub mod connections;
pub mod db;
pub mod dto;
pub mod error;
pub mod id;
pub mod idempotency;
pub mod operations;
pub mod operator;
pub mod organizations;
pub mod outbox;
pub mod pages;
pub mod pagination;
pub mod provisioning;
pub mod quotas;
pub mod rate_limit;
pub mod releases;
pub mod repositories;
pub mod routes;
pub mod runtime;
pub mod secrets;
pub mod server;
pub mod templates;
pub mod tenants;
pub mod zep;

pub use config::{ManagerConfig, Mode};
pub use db::Db;
pub use error::{ApiError, ManagerError};
pub use id::*;
pub use secrets::{SecretProvider, SecretRef};
