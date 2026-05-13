use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub enum ChannelType {
    Slack,
    Discord,
    WhatsApp,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum MessageType {
    WorkflowStarted,
    AgentAssigned,
    AgentCompleted,
    WorkflowError,
    QuestionToHuman,
    StatusUpdate,
    ApprovalRequest,
    PauseWorkflow,
    ResumeWorkflow,
    AnswerQuestion,
    ApproveCommand,
    RerouteAgent,
    BlockAgent,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct NexusMessage {
    pub message_type: MessageType,
    pub ticket_id: Option<String>,
    pub worker_id: Option<String>,
    pub content: String,
    pub metadata: serde_json::Value,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
}

impl NexusMessage {
    pub fn workflow_started(ticket_id: &str, worker_id: &str, content: &str, issue_url: Option<&str>) -> Self {
        Self {
            message_type: MessageType::WorkflowStarted,
            ticket_id: Some(ticket_id.to_string()),
            worker_id: Some(worker_id.to_string()),
            content: content.to_string(),
            metadata: serde_json::json!({ "issue_url": issue_url }),
            timestamp: Utc::now(),
        }
    }

    pub fn agent_assigned(worker_id: &str, ticket_id: &str, content: &str) -> Self {
        Self {
            message_type: MessageType::AgentAssigned,
            ticket_id: Some(ticket_id.to_string()),
            worker_id: Some(worker_id.to_string()),
            content: content.to_string(),
            metadata: serde_json::Value::Null,
            timestamp: Utc::now(),
        }
    }

    pub fn agent_completed(worker_id: &str, ticket_id: &str, content: &str) -> Self {
        Self {
            message_type: MessageType::AgentCompleted,
            ticket_id: Some(ticket_id.to_string()),
            worker_id: Some(worker_id.to_string()),
            content: content.to_string(),
            metadata: serde_json::Value::Null,
            timestamp: Utc::now(),
        }
    }

    pub fn workflow_error(worker_id: &str, ticket_id: Option<&str>, content: &str) -> Self {
        Self {
            message_type: MessageType::WorkflowError,
            ticket_id: ticket_id.map(|s| s.to_string()),
            worker_id: Some(worker_id.to_string()),
            content: content.to_string(),
            metadata: serde_json::Value::Null,
            timestamp: Utc::now(),
        }
    }

    pub fn question_to_human(ticket_id: &str, question: &str, options: &[&str]) -> Self {
        Self {
            message_type: MessageType::QuestionToHuman,
            ticket_id: Some(ticket_id.to_string()),
            worker_id: None,
            content: question.to_string(),
            metadata: serde_json::json!({ "options": options }),
            timestamp: Utc::now(),
        }
    }

    pub fn status_update(content: &str) -> Self {
        Self {
            message_type: MessageType::StatusUpdate,
            ticket_id: None,
            worker_id: None,
            content: content.to_string(),
            metadata: serde_json::Value::Null,
            timestamp: Utc::now(),
        }
    }

    pub fn approval_request(worker_id: &str, command: &str, reason: &str) -> Self {
        Self {
            message_type: MessageType::ApprovalRequest,
            ticket_id: None,
            worker_id: Some(worker_id.to_string()),
            content: format!("{}: {}", command, reason),
            metadata: serde_json::json!({ "command": command, "reason": reason }),
            timestamp: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanMessage {
    pub user_id: String,
    pub channel_id: String,
    pub thread_ts: Option<String>,
    pub text: String,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
}

impl HumanMessage {
    pub fn new(user_id: &str, channel_id: &str, text: &str) -> Self {
        Self {
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            thread_ts: None,
            text: text.to_string(),
            timestamp: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct HumanCommand {
    pub command: MessageType,
    pub ticket_id: Option<String>,
    pub worker_id: Option<String>,
    pub payload: Option<String>,
    pub user_id: String,
    pub channel_id: String,
    pub thread_ts: Option<String>,
    #[serde(default = "Utc::now")]
    pub timestamp: DateTime<Utc>,
}

impl HumanCommand {
    pub fn pause_workflow(ticket_id: &str, user_id: &str, channel_id: &str) -> Self {
        Self {
            command: MessageType::PauseWorkflow,
            ticket_id: Some(ticket_id.to_string()),
            worker_id: None,
            payload: None,
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            thread_ts: None,
            timestamp: Utc::now(),
        }
    }

    pub fn resume_workflow(ticket_id: &str, user_id: &str, channel_id: &str) -> Self {
        Self {
            command: MessageType::ResumeWorkflow,
            ticket_id: Some(ticket_id.to_string()),
            worker_id: None,
            payload: None,
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            thread_ts: None,
            timestamp: Utc::now(),
        }
    }

    pub fn approve_command(worker_id: &str, user_id: &str, channel_id: &str) -> Self {
        Self {
            command: MessageType::ApproveCommand,
            ticket_id: None,
            worker_id: Some(worker_id.to_string()),
            payload: None,
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            thread_ts: None,
            timestamp: Utc::now(),
        }
    }

    pub fn block_agent(worker_id: &str, reason: &str, user_id: &str, channel_id: &str) -> Self {
        Self {
            command: MessageType::BlockAgent,
            ticket_id: None,
            worker_id: Some(worker_id.to_string()),
            payload: Some(reason.to_string()),
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            thread_ts: None,
            timestamp: Utc::now(),
        }
    }

    pub fn reroute_agent(from_worker: &str, to_worker: &str, user_id: &str, channel_id: &str) -> Self {
        Self {
            command: MessageType::RerouteAgent,
            ticket_id: None,
            worker_id: Some(from_worker.to_string()),
            payload: Some(to_worker.to_string()),
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            thread_ts: None,
            timestamp: Utc::now(),
        }
    }

    pub fn answer_question(ticket_id: &str, answer: &str, user_id: &str, channel_id: &str) -> Self {
        Self {
            command: MessageType::AnswerQuestion,
            ticket_id: Some(ticket_id.to_string()),
            worker_id: None,
            payload: Some(answer.to_string()),
            user_id: user_id.to_string(),
            channel_id: channel_id.to_string(),
            thread_ts: None,
            timestamp: Utc::now(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ChatConfig {
    pub slack_bot_token: Option<String>,
    pub slack_channel_id: Option<String>,
    pub slack_signing_secret: Option<String>,
    pub discord_bot_token: Option<String>,
    pub discord_channel_id: Option<String>,
    pub whatsapp_api_key: Option<String>,
    pub whatsapp_phone_number: Option<String>,
    pub whatsapp_api_url: Option<String>,
    pub enabled: bool,
    pub dev_mode: bool,
}

impl ChatConfig {
    pub fn from_env() -> Self {
        Self {
            slack_bot_token: std::env::var("NEXUS_CHAT_SLACK_BOT_TOKEN").ok(),
            slack_channel_id: std::env::var("NEXUS_CHAT_SLACK_CHANNEL_ID").ok(),
            slack_signing_secret: std::env::var("NEXUS_CHAT_SLACK_SIGNING_SECRET").ok(),
            discord_bot_token: std::env::var("NEXUS_CHAT_DISCORD_BOT_TOKEN").ok(),
            discord_channel_id: std::env::var("NEXUS_CHAT_DISCORD_CHANNEL_ID").ok(),
            whatsapp_api_key: std::env::var("NEXUS_CHAT_WHATSAPP_API_KEY").ok(),
            whatsapp_phone_number: std::env::var("NEXUS_CHAT_WHATSAPP_PHONE_NUMBER").ok(),
            whatsapp_api_url: std::env::var("NEXUS_CHAT_WHATSAPP_API_URL")
                .ok()
                .or_else(|| Some("https://graph.facebook.com/v18.0".to_string())),
            enabled: std::env::var("NEXUS_CHAT_ENABLED")
                .ok()
                .map(|v| v.to_lowercase() == "true")
                .unwrap_or(false),
            dev_mode: std::env::var("NEXUS_CHAT_DEV_MODE")
                .ok()
                .map(|v| v.to_lowercase() == "true")
                .unwrap_or(false),
        }
    }

    pub fn is_configured(&self) -> bool {
        self.enabled
            && (self.dev_mode
                || self.slack_bot_token.is_some()
                || self.discord_bot_token.is_some()
                || self.whatsapp_api_key.is_some())
    }

    pub fn active_channels(&self) -> Vec<ChannelType> {
        let mut channels = Vec::new();
        if self.slack_bot_token.is_some() && self.slack_channel_id.is_some() {
            channels.push(ChannelType::Slack);
        }
        if self.discord_bot_token.is_some() && self.discord_channel_id.is_some() {
            channels.push(ChannelType::Discord);
        }
        if self.whatsapp_api_key.is_some() && self.whatsapp_phone_number.is_some() {
            channels.push(ChannelType::WhatsApp);
        }
        if self.dev_mode || channels.is_empty() {
            channels.push(ChannelType::Slack);
        }
        channels
    }
}

impl Default for ChatConfig {
    fn default() -> Self {
        Self {
            slack_bot_token: None,
            slack_channel_id: None,
            slack_signing_secret: None,
            discord_bot_token: None,
            discord_channel_id: None,
            whatsapp_api_key: None,
            whatsapp_phone_number: None,
            whatsapp_api_url: None,
            enabled: false,
            dev_mode: false,
        }
    }
}
