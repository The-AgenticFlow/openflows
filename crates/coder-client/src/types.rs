// crates/coder-client/src/types.rs
//! Types for the Coder client API.

use serde::{Deserialize, Serialize};

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
