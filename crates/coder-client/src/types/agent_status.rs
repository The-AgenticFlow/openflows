use serde::{Deserialize, Serialize};

/// Status of the agent connection inside a Coder workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Connecting,
    Connected,
    Disconnected,
    Timeout,
    // Coder adds statuses over time; an unseen one must not fail a readiness poll.
    #[serde(untagged)]
    Unknown(String),
}

impl Default for AgentStatus {
    fn default() -> Self {
        AgentStatus::Unknown("unset".to_string())
    }
}

impl AgentStatus {
    pub fn from_build_status(s: &str) -> Self {
        match s {
            "running" => AgentStatus::Connected,
            "stopped" | "stopping" => AgentStatus::Disconnected,
            "timeout" => AgentStatus::Timeout,
            other => AgentStatus::Unknown(other.to_string()),
        }
    }
}
