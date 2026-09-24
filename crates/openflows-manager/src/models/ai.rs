//! Models for AI Provider, Model management, and OpenFlows Model Assignment Policy (Issue #290).

use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Public, safe summary of an AI Provider with secrets strictly redacted.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiProviderSummary {
    pub id: String,
    #[serde(rename = "type")]
    pub provider_type: String,
    pub name: String,
    pub display_name: String,
    pub base_url: String,
    pub enabled: bool,
    pub has_api_key: bool,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to create an AI Provider. Secrets are write-only.
#[derive(Debug, Clone, Deserialize)]
pub struct AiProviderCreateRequest {
    #[serde(alias = "provider_type", alias = "type")]
    pub provider_type: String,
    pub name: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_keys: Option<Vec<String>>,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_enabled() -> bool {
    true
}

/// Request to update an existing AI Provider.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AiProviderUpdateRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub base_url: Option<String>,
    #[serde(default)]
    pub api_key: Option<String>,
    #[serde(default)]
    pub api_keys: Option<Vec<String>>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Public representation of an AI Chat Model.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct AiModelSummary {
    pub id: String,
    pub ai_provider_id: String,
    pub model: String,
    pub display_name: String,
    pub enabled: bool,
    pub is_default: bool,
    pub context_limit: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compression_threshold: Option<u64>,
    pub created_at: String,
    pub updated_at: String,
}

/// Request to register a Chat Model with an AI Provider.
#[derive(Debug, Clone, Deserialize)]
pub struct AiModelCreateRequest {
    pub ai_provider_id: String,
    pub model: String,
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default = "default_context_limit")]
    pub context_limit: u64,
    #[serde(default)]
    pub compression_threshold: Option<u64>,
    #[serde(default)]
    pub is_default: bool,
    #[serde(default = "default_enabled")]
    pub enabled: bool,
}

fn default_context_limit() -> u64 {
    128_000
}

/// Request to update an existing Chat Model.
#[derive(Debug, Clone, Deserialize, Default)]
pub struct AiModelUpdateRequest {
    #[serde(default)]
    pub display_name: Option<String>,
    #[serde(default)]
    pub context_limit: Option<u64>,
    #[serde(default)]
    pub compression_threshold: Option<u64>,
    #[serde(default)]
    pub is_default: Option<bool>,
    #[serde(default)]
    pub enabled: Option<bool>,
}

/// Model assignment policy mapping agent roles to explicit models.
/// Supports optional global and tenant-scoped configurations.
#[derive(Debug, Clone, Serialize, Deserialize, Default, PartialEq, Eq)]
pub struct ModelPolicy {
    /// Default fallback model when no role-specific model is defined.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_model: Option<String>,
    /// Explicit model assignments by agent role (e.g. "forge" -> "claude-3-7-sonnet").
    #[serde(default)]
    pub roles: HashMap<String, String>,
}

/// Resolved model for a specific role and context.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct ResolvedModel {
    pub role: String,
    pub model: String,
    pub source: ModelResolutionSource,
}

/// Where the resolved model came from.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum ModelResolutionSource {
    /// Found an explicit role assignment in the policy.
    RolePolicy,
    /// Fallen back to the policy's default_model.
    DefaultPolicy,
    /// Fallen back to Coder's deployment-default model.
    CoderDefault,
}
