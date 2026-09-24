//! Models for tenant lifecycle APIs.

use serde::{Deserialize, Serialize};

/// High-level summary of a tenant.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantSummary {
    pub name: String,
    pub repository: Option<String>,
}

/// Detailed tenant state.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct TenantDetail {
    pub name: String,
    pub repository: Option<String>,
    pub registry: Option<serde_json::Value>,
    pub key_count: usize,
    pub ticket_count: usize,
    pub active_workers: usize,
}

/// Request to create/register a tenant.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantCreateRequest {
    pub repo: String,
    pub name: Option<String>,
    #[serde(default = "default_fleet")]
    pub fleet: u32,
}

fn default_fleet() -> u32 {
    1
}

/// Response returned on successful tenant creation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantCreateResponse {
    pub tenant: String,
    pub repository: String,
    pub fleet: u32,
    pub workspace_id: String,
    pub message: String,
}

/// Request parameters for cleaning tenant runtime state.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TenantCleanRequest {
    #[serde(default)]
    pub reset_all: bool,
}

/// Response returned on tenant clean.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantCleanResponse {
    pub tenant: String,
    pub reset_tickets_count: usize,
    pub cleared_recovery_counters: usize,
    pub message: String,
}

/// Query parameters for tenant deletion.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct TenantRemoveQuery {
    #[serde(default = "default_purge")]
    pub purge: bool,
}

fn default_purge() -> bool {
    true
}

/// Response returned on tenant removal.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantRemoveResponse {
    pub tenant: String,
    pub purged_keys_count: usize,
    pub message: String,
}
