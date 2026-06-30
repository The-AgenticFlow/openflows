// crates/coder-client/src/types.rs
//! Types for the Coder client API.

use serde::{Deserialize, Serialize};

/// Paginated response for the Coder /api/v2/users endpoint.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsersResponse {
    #[serde(default)]
    pub users: Vec<CoderUser>,
    #[serde(default)]
    pub total_count: u64,
}

/// Output from a command executed in a Coder workspace.
#[derive(Debug, Clone)]
pub struct CommandOutput {
    pub stdout: String,
    pub stderr: String,
    pub exit_code: i32,
}

/// A Coder user.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderUser {
    pub id: String,
    #[serde(default)]
    pub username: String,
    #[serde(default)]
    pub email: String,
}

/// A Coder template.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderTemplate {
    pub id: String,
    #[serde(default)]
    pub name: String,
}

/// Request to create a workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CreateWorkspaceRequest {
    pub template_name: String,
    pub name: String,
    pub parameters: serde_json::Value,
}

/// A Coder workspace.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderWorkspace {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owner_name: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub latest_build: Option<WorkspaceBuild>,
}

impl CoderWorkspace {
    pub fn is_running(&self) -> bool {
        self.latest_build
            .as_ref()
            .map(|b| b.status == "running")
            .unwrap_or(false)
            || self.status == "running"
    }

    pub fn workspace_status(&self) -> WorkspaceStatus {
        WorkspaceStatus::from_status_str(&self.status)
    }

    pub fn agent_status(&self) -> AgentStatus {
        match &self.latest_build {
            Some(build) => AgentStatus::from_build_status(&build.status),
            None => AgentStatus::Unknown,
        }
    }
}

/// Status of a Coder workspace, derived from the workspace's `status` field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceStatus {
    Pending,
    Starting,
    Running,
    Stopping,
    Stopped,
    Failed,
    Deleting,
    Deleted,
    Unknown(String),
}

impl WorkspaceStatus {
    pub fn from_status_str(s: &str) -> Self {
        match s {
            "pending" => WorkspaceStatus::Pending,
            "starting" => WorkspaceStatus::Starting,
            "running" => WorkspaceStatus::Running,
            "stopping" => WorkspaceStatus::Stopping,
            "stopped" => WorkspaceStatus::Stopped,
            "failed" => WorkspaceStatus::Failed,
            "deleting" => WorkspaceStatus::Deleting,
            "deleted" => WorkspaceStatus::Deleted,
            other => WorkspaceStatus::Unknown(other.to_string()),
        }
    }
}

/// Status of the agent inside a Coder workspace, derived from the latest build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Connected,
    Disconnected,
    Timeout,
    Unknown,
}

impl AgentStatus {
    pub fn from_build_status(s: &str) -> Self {
        match s {
            "running" => AgentStatus::Connected,
            "stopped" | "stopping" => AgentStatus::Disconnected,
            "timeout" => AgentStatus::Timeout,
            _ => AgentStatus::Unknown,
        }
    }
}

/// A workspace build (status info).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkspaceBuild {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub transition: Option<String>,
}

/// A Coder API key (token).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CoderApiKey {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub key: String,
}

// ── Chats API types (Phase 3) ─────────────────────────────────────────────

/// The lifecycle status of a Coder Chat session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ChatStatus {
    Pending,
    Running,
    Waiting,
    Error,
    RequiresAction,
}

impl ChatStatus {
    /// Parse a status string from the Coder API response.
    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => ChatStatus::Pending,
            "running" => ChatStatus::Running,
            "waiting" => ChatStatus::Waiting,
            "error" => ChatStatus::Error,
            "requires_action" => ChatStatus::RequiresAction,
            other => {
                tracing::warn!(raw = other, "Unknown chat status, treating as Running");
                ChatStatus::Running
            }
        }
    }

    /// Convert back to string for comparison / logging.
    pub fn as_str(&self) -> &'static str {
        match self {
            ChatStatus::Pending => "pending",
            ChatStatus::Running => "running",
            ChatStatus::Waiting => "waiting",
            ChatStatus::Error => "error",
            ChatStatus::RequiresAction => "requires_action",
        }
    }
}

/// A single content element in a Chat input part.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ChatInputPart {
    Text { text: String },
    File { file_id: String },
    FileReference { file_id: String },
}

impl ChatInputPart {
    /// Convenience constructor for plain text content.
    pub fn text(content: impl Into<String>) -> Self {
        ChatInputPart::Text { text: content.into() }
    }
}

/// Request body for creating a new Chat session.
#[derive(Debug, Clone, Serialize)]
pub struct CreateChatRequest {
    /// Workspace ID to run the chat in.
    pub workspace_id: String,
    /// The model config ID (from `/api/experimental/chats/models`).
    pub model_config_id: String,
    /// The initial prompt / message content.
    pub content: Vec<ChatInputPart>,
    /// Key-value labels for filtering and querying chats.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub labels: Option<serde_json::Map<String, serde_json::Value>>,
}

/// A Coder Chat session.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Chat {
    pub id: String,
    #[serde(default)]
    pub organization_id: String,
    #[serde(default)]
    pub owner_id: String,
    #[serde(default)]
    pub workspace_id: String,
    #[serde(default)]
    pub model_config_id: String,
    #[serde(default, rename = "status")]
    pub status_raw: String,
    #[serde(default)]
    pub title: String,
    #[serde(default, rename = "created_at")]
    pub created_at_raw: String,
    #[serde(default, rename = "updated_at")]
    pub updated_at_raw: String,
    /// Labels applied at creation time (e.g. ticket, role, flow).
    #[serde(default)]
    pub labels: serde_json::Map<String, serde_json::Value>,
}

impl Chat {
    /// Get the parsed status enum.
    pub fn status(&self) -> ChatStatus {
        ChatStatus::from_str(&self.status_raw)
    }
}

/// A message in a Chat conversation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatMessage {
    pub id: String,
    #[serde(default)]
    pub chat_id: String,
    #[serde(default, rename = "role")]
    pub role: String,
    #[serde(default, rename = "content")]
    pub content_raw: serde_json::Value,
    #[serde(default, rename = "created_at")]
    pub created_at_raw: String,
}

/// A model returned from `GET /api/experimental/chats/models`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ModelInfo {
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default, rename = "display_name")]
    pub display_name: String,
    #[serde(default)]
    pub provider: String,
}

/// The diff status payload attached to a forge chat.
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct DiffStatus {
    #[serde(default)]
    pub pr_number: Option<u64>,
    #[serde(default)]
    pub head_branch: Option<String>,
    #[serde(default, rename = "changed_files")]
    pub changed_files_count: Option<u64>,
    #[serde(default, rename = "pull_request_state")]
    pub pull_request_state: Option<String>,
    #[serde(default, rename = "pull_request_title")]
    pub pull_request_title: Option<String>,
    #[serde(default)]
    pub approved: Option<bool>,
    #[serde(default, rename = "changes_requested")]
    pub changes_requested: Option<bool>,
}

// ── Chat label schema constants (Phase 3, Task 3.5) ──────────────────────

/// Label key for the ticket ID associated with a chat.
pub const CHAT_LABEL_TICKET: &str = "ticket_id";

/// Label key for the agent role associated with a chat.
pub const CHAT_LABEL_ROLE: &str = "role";

/// Label key for the orchestrator flow associated with a chat.
pub const CHAT_LABEL_FLOW: &str = "flow";

/// Build a labels map for a ticket-scoped chat.
pub fn build_chat_labels(ticket_id: &str, role: &str, flow: &str) -> serde_json::Map<String, serde_json::Value> {
    let mut map = serde_json::Map::new();
    map.insert(CHAT_LABEL_TICKET.to_string(), serde_json::Value::String(ticket_id.to_string()));
    map.insert(CHAT_LABEL_ROLE.to_string(), serde_json::Value::String(role.to_string()));
    map.insert(CHAT_LABEL_FLOW.to_string(), serde_json::Value::String(flow.to_string()));
    map
}
