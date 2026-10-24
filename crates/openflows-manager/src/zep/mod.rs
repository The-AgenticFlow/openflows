//! Zep context-layer client for OpenFlows.
//!
//! Zep is the context layer that gives the OpenFlows agents durable memory:
//! users, conversation threads, and a temporal knowledge graph. This module is
//! a small, typed REST client over Zep's v3 HTTP API (`api.getzep.com/api/v2`),
//! implemented directly with the workspace `reqwest` dependency because no
//! official Rust SDK exists.
//!
//! The client is fail-open by design: Zep enriches agent context but is never
//! on the critical path of signup or the chat relay. Integration points call
//! into [`ZepService`], which swallows errors and logs them rather than
//! aborting the surrounding operation.
//!
//! The org-wide graph ontology and the shared organizational knowledge graph
//! (`graph_id`) are defined in [`ontology`].

pub mod ontology;
pub mod service;

use crate::error::ManagerError;
use serde::{Deserialize, Serialize};
use std::time::Duration;

/// The default Zep v3 REST API base path.
pub const DEFAULT_ZEP_API_URL: &str = "https://api.getzep.com/api/v2";
/// A single HTTP request timeout for Zep calls (they are best-effort).
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

/// A thin, typed REST client for Zep's v3 HTTP API.
///
/// Cheap to clone; each method issues an HTTP request over a shared
/// `reqwest::Client`. Methods return [`ManagerError::Service`] on any failure
/// so callers can treat Zep as best-effort.
#[derive(Clone)]
pub struct ZepClient {
    http: reqwest::Client,
    api_key: String,
    /// Base URL including the `/api/v2` suffix.
    base_url: String,
}

impl ZepClient {
    /// Build a client from an API key and an optional base URL override.
    pub fn new(api_key: impl Into<String>, base_url: Option<String>) -> Self {
        ZepClient {
            http: reqwest::Client::builder()
                .timeout(REQUEST_TIMEOUT)
                .build()
                .unwrap_or_default(),
            api_key: api_key.into(),
            base_url: base_url.unwrap_or_else(|| DEFAULT_ZEP_API_URL.to_string()),
        }
    }

    fn url(&self, path: &str) -> String {
        format!(
            "{}/{}",
            self.base_url.trim_end_matches('/'),
            path.trim_start_matches('/')
        )
    }

    async fn post<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, ManagerError> {
        let response = self
            .http
            .post(self.url(path))
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(zep_err)?;
        let status = response.status();
        let text = response.text().await.map_err(zep_err)?;
        if !status.is_success() {
            return Err(zep_err(anyhow::anyhow!(
                "Zep {path} returned {status}: {}",
                sanitize(&text)
            )));
        }
        serde_json::from_str(&text).map_err(|e| zep_err(anyhow::anyhow!("Zep {path}: {e}")))
    }

    async fn put<T: Serialize, R: for<'de> Deserialize<'de>>(
        &self,
        path: &str,
        body: &T,
    ) -> Result<R, ManagerError> {
        let response = self
            .http
            .put(self.url(path))
            .bearer_auth(&self.api_key)
            .json(body)
            .send()
            .await
            .map_err(zep_err)?;
        let status = response.status();
        let text = response.text().await.map_err(zep_err)?;
        if !status.is_success() {
            return Err(zep_err(anyhow::anyhow!(
                "Zep {path} returned {status}: {}",
                sanitize(&text)
            )));
        }
        serde_json::from_str(&text).map_err(|e| zep_err(anyhow::anyhow!("Zep {path}: {e}")))
    }

    async fn get<R: for<'de> Deserialize<'de>>(&self, path: &str) -> Result<R, ManagerError> {
        let response = self
            .http
            .get(self.url(path))
            .bearer_auth(&self.api_key)
            .send()
            .await
            .map_err(zep_err)?;
        let status = response.status();
        let text = response.text().await.map_err(zep_err)?;
        if !status.is_success() {
            return Err(zep_err(anyhow::anyhow!(
                "Zep {path} returned {status}: {}",
                sanitize(&text)
            )));
        }
        serde_json::from_str(&text).map_err(|e| zep_err(anyhow::anyhow!("Zep {path}: {e}")))
    }

    /// Create a Zep user keyed by our internal user id. `user_id` must be our
    /// internal id (never a Zep UUID); Zep returns the authoritative user UUID.
    pub async fn user_add(&self, req: &CreateUserRequest) -> Result<User, ManagerError> {
        self.post("users", req).await
    }

    /// Create a Zep thread tied to a user. `thread_id` is our conversation id.
    pub async fn thread_create(&self, req: &CreateThreadRequest) -> Result<Thread, ManagerError> {
        self.post("threads", req).await
    }

    /// Append messages to an existing thread.
    pub async fn thread_add_messages(
        &self,
        thread_id: &str,
        req: &AddThreadMessagesRequest,
    ) -> Result<AddThreadMessagesResponse, ManagerError> {
        self.post(&format!("threads/{thread_id}/messages"), req)
            .await
    }

    /// Retrieve the assembled user Context Block for a thread.
    pub async fn thread_get_user_context(
        &self,
        thread_id: &str,
    ) -> Result<ThreadContext, ManagerError> {
        self.get(&format!("threads/{thread_id}/context")).await
    }

    /// Add business data (episodes) to a user graph or a shared graph. Target
    /// either a `user_id` or a `graph_id` (never both for the same call).
    pub async fn graph_add(&self, req: &AddDataRequest) -> Result<Episode, ManagerError> {
        self.post("graph", req).await
    }

    /// Set the custom entity/edge ontology for the whole project or for a
    /// specific set of user/graph ids.
    pub async fn graph_set_ontology(
        &self,
        req: &SetOntologyRequest,
    ) -> Result<OntologyResponse, ManagerError> {
        self.put("graph/set-ontology", req).await
    }
}

fn zep_err(e: impl Into<anyhow::Error>) -> ManagerError {
    ManagerError::Service(e.into())
}

/// Never leak Zep response bodies (which may echo user data or credentials)
/// into logs or error messages beyond a bounded preview.
fn sanitize(text: &str) -> String {
    let trimmed = text.trim();
    if trimmed.chars().count() > 120 {
        let mut out: String = trimmed.chars().take(120).collect();
        out.push('…');
        out
    } else {
        trimmed.to_string()
    }
}

// ---------------------------------------------------------------------------
// Request / response types (v3 REST API).
// ---------------------------------------------------------------------------

/// Create a user (`POST /api/v2/users`).
#[derive(Debug, Clone, Serialize)]
pub struct CreateUserRequest {
    /// Our internal user id; never a Zep UUID.
    pub user_id: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub first_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub last_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub disable_default_ontology: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// The user object Zep returns.
#[derive(Debug, Clone, Deserialize)]
pub struct User {
    pub user_id: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub graph_uuid: Option<String>,
    #[serde(default)]
    pub email: Option<String>,
    #[serde(default)]
    pub first_name: Option<String>,
    #[serde(default)]
    pub last_name: Option<String>,
}

/// Create a thread (`POST /api/v2/threads`).
#[derive(Debug, Clone, Serialize)]
pub struct CreateThreadRequest {
    /// Our conversation id; never a Zep UUID.
    pub thread_id: String,
    /// Our internal user id.
    pub user_id: String,
}

/// The thread object Zep returns.
#[derive(Debug, Clone, Deserialize)]
pub struct Thread {
    #[serde(default)]
    pub thread_id: Option<String>,
    #[serde(default)]
    pub user_id: Option<String>,
    #[serde(default)]
    pub user_uuid: Option<String>,
    #[serde(default)]
    pub uuid: Option<String>,
    #[serde(default)]
    pub graph_uuid: Option<String>,
}

/// A message to append to a thread.
#[derive(Debug, Clone, Serialize)]
pub struct ThreadMessage {
    pub role: String,
    pub content: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub created_at: Option<String>,
}

/// Add messages to a thread (`POST /api/v2/threads/{threadId}/messages`).
#[derive(Debug, Clone, Serialize)]
pub struct AddThreadMessagesRequest {
    pub messages: Vec<ThreadMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ignore_roles: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_ontology: Option<bool>,
}

/// Response from adding messages.
#[derive(Debug, Clone, Deserialize)]
pub struct AddThreadMessagesResponse {
    #[serde(default)]
    pub context: Option<String>,
    #[serde(default)]
    pub message_uuids: Vec<String>,
    #[serde(default)]
    pub task_id: Option<String>,
}

/// User Context Block (`GET /api/v2/threads/{threadId}/context`).
#[derive(Debug, Clone, Deserialize)]
pub struct ThreadContext {
    #[serde(default)]
    pub context: Option<String>,
}

/// The kind of data added to a graph.
#[derive(Debug, Clone, Copy, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GraphDataType {
    Text,
    Json,
    Message,
    FactTriple,
}

/// Add data to a graph (`POST /api/v2/graph`). Target exactly one of
/// `user_id` (a user graph) or `graph_id` (a shared/org graph).
#[derive(Debug, Clone, Serialize)]
pub struct AddDataRequest {
    pub data: String,
    #[serde(rename = "type")]
    pub type_: GraphDataType,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_description: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub strict_ontology: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub metadata: Option<serde_json::Map<String, serde_json::Value>>,
}

/// An episode added to a graph.
#[derive(Debug, Clone, Deserialize)]
pub struct Episode {
    pub uuid: Option<String>,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub processed: Option<bool>,
    #[serde(default)]
    pub task_id: Option<String>,
}

/// An entity type definition for the ontology.
#[derive(Debug, Clone, Serialize)]
pub struct EntityTypeDef {
    pub description: String,
    pub fields: serde_json::Map<String, serde_json::Value>,
}

/// An edge type definition for the ontology, with source/target constraints.
#[derive(Debug, Clone, Serialize)]
pub struct EdgeTypeDef {
    pub description: String,
    pub fields: serde_json::Map<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source_targets: Option<Vec<SourceTarget>>,
}

/// A single source/target constraint for an edge type.
#[derive(Debug, Clone, Serialize)]
pub struct SourceTarget {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
}

/// Set graph ontology (`PUT /api/v2/graph/set-ontology`).
#[derive(Debug, Clone, Serialize)]
pub struct SetOntologyRequest {
    pub entities: serde_json::Map<String, serde_json::Value>,
    pub edges: serde_json::Map<String, serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub user_ids: Option<Vec<String>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub graph_ids: Option<Vec<String>>,
}

/// Response from setting the ontology.
#[derive(Debug, Clone, Deserialize)]
pub struct OntologyResponse {
    #[serde(default)]
    pub message: Option<String>,
}
