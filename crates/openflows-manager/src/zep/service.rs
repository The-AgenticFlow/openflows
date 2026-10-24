//! `ZepService` — best-effort Zep integration bound to the Manager.
//!
//! Zep enriches the OpenFlows agents' context but is deliberately non-fatal:
//! a Zep outage must never block a signup, a chat relay, or tenant
//! provisioning. Every method here returns `()` and swallows errors (logging a
//! warning) so callers can invoke it from critical paths without changing
//! their error handling. A `None` service (Zep not configured) is a no-op.

use crate::error::ManagerError;
use crate::zep::{
    AddDataRequest, AddThreadMessagesRequest, CreateThreadRequest, CreateUserRequest,
    GraphDataType, ThreadMessage, ZepClient,
};
use tracing::warn;

/// The Zep integration facade held by [`ManagerServices`].
///
/// When Zep is not configured this is `None` and all operations are no-ops.
/// When configured it owns the shared HTTP client and the org graph id.
#[derive(Clone)]
pub struct ZepService {
    client: ZepClient,
    /// The shared organization-knowledge graph id.
    org_graph_id: String,
}

impl ZepService {
    pub fn new(client: ZepClient, org_graph_id: String) -> Self {
        ZepService {
            client,
            org_graph_id,
        }
    }

    /// Access the underlying client (used by startup seeding).
    pub fn client(&self) -> &ZepClient {
        &self.client
    }

    /// The shared org-knowledge graph id.
    pub fn org_graph_id(&self) -> &str {
        &self.org_graph_id
    }

    /// Create a Zep user for a newly-registered application user.
    ///
    /// Idempotent: Zep keys by our internal `user_id`, so a retry after a
    /// network blip does not create a duplicate. Best-effort; failures are
    /// logged, not returned.
    pub async fn user_add(
        &self,
        user_id: &str,
        first_name: &str,
        last_name: &str,
        email: Option<&str>,
    ) {
        if user_id.is_empty() {
            return;
        }
        let req = CreateUserRequest {
            user_id: user_id.to_string(),
            first_name: Some(first_name.to_string()),
            last_name: Some(last_name.to_string()),
            email: email.map(str::to_string),
            disable_default_ontology: Some(true),
            metadata: None,
        };
        if let Err(e) = self.client.user_add(&req).await {
            warn!(user_id, error = %e, "Zep user.add failed");
        }
    }

    /// Create a thread for a conversation (`thread_id` = our conversation id).
    /// Best-effort; failures are logged, not returned.
    pub async fn thread_create(&self, thread_id: &str, user_id: &str) {
        if thread_id.is_empty() || user_id.is_empty() {
            return;
        }
        let req = CreateThreadRequest {
            thread_id: thread_id.to_string(),
            user_id: user_id.to_string(),
        };
        if let Err(e) = self.client.thread_create(&req).await {
            warn!(thread_id, user_id, error = %e, "Zep thread.create failed");
        }
    }

    /// Append user/assistant messages to a thread. Best-effort.
    pub async fn thread_add_messages(&self, thread_id: &str, messages: Vec<ThreadMessage>) {
        if thread_id.is_empty() || messages.is_empty() {
            return;
        }
        let req = AddThreadMessagesRequest {
            messages,
            ignore_roles: None,
            strict_ontology: Some(true),
        };
        if let Err(e) = self.client.thread_add_messages(thread_id, &req).await {
            warn!(thread_id, error = %e, "Zep thread.add_messages failed");
        }
    }

    /// Add shared organizational knowledge to the org graph (policies, product
    /// facts, internal docs). Any text/JSON is fine; Zep extracts structured
    /// entities per the declared ontology. Best-effort.
    pub async fn org_knowledge_add(&self, data: String, source_description: &str) {
        if data.trim().is_empty() {
            return;
        }
        let req = AddDataRequest {
            data,
            type_: GraphDataType::Text,
            user_id: None,
            graph_id: Some(self.org_graph_id.clone()),
            source_description: Some(source_description.to_string()),
            strict_ontology: Some(true),
            metadata: None,
        };
        if let Err(e) = self.client.graph_add(&req).await {
            warn!(graph_id = %self.org_graph_id, error = %e, "Zep org graph.add failed");
        }
    }

    /// Add user-specific business data to a user's graph (orders, tickets,
    /// events). Target the user graph via `user_id`. Best-effort.
    pub async fn user_business_data_add(
        &self,
        user_id: &str,
        data: String,
        source_description: &str,
    ) {
        if user_id.is_empty() || data.trim().is_empty() {
            return;
        }
        let req = AddDataRequest {
            data,
            type_: GraphDataType::Json,
            user_id: Some(user_id.to_string()),
            graph_id: None,
            source_description: Some(source_description.to_string()),
            strict_ontology: Some(true),
            metadata: None,
        };
        if let Err(e) = self.client.graph_add(&req).await {
            warn!(user_id, error = %e, "Zep graph.add failed");
        }
    }

    /// Apply the custom graph ontology project-wide.
    pub async fn apply_project_ontology(&self) -> Result<(), ManagerError> {
        let req = crate::zep::ontology::ontology_request();
        self.client.graph_set_ontology(&req).await.map(|_| ())
    }

    /// Apply the custom graph ontology to the shared org knowledge graph.
    pub async fn apply_org_ontology(&self) -> Result<(), ManagerError> {
        let mut req = crate::zep::ontology::org_graph_ontology_request();
        req.graph_ids = Some(vec![self.org_graph_id.clone()]);
        self.client.graph_set_ontology(&req).await.map(|_| ())
    }
}
