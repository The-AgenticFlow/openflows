use anyhow::{bail, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::{ChatClient, ChatConfig, HumanCommand, MessageType, NexusMessage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SlackMessage {
    pub ts: String,
    pub text: String,
    pub user: Option<String>,
    pub channel: String,
    pub thread_ts: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SlackHistoryResponse {
    pub ok: bool,
    pub messages: Vec<SlackMessage>,
    pub error: Option<String>,
}

pub struct SlackClient {
    client: Client,
    config: ChatConfig,
}

impl SlackClient {
    pub fn new(config: ChatConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
    }

    pub async fn send_message(&self, msg: &NexusMessage) -> Result<()> {
        let token = self.config.slack_bot_token.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Slack bot token not configured")
        })?;
        let channel = self.config.slack_channel_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Slack channel ID not configured")
        })?;

        let text = self.format_message(msg);
        let blocks = self.build_blocks(msg);

        let response = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(token)
            .json(&json!({
                "channel": channel,
                "text": text,
                "blocks": blocks,
            }))
            .send()
            .await?;

        let body: serde_json::Value = response.json().await?;
        if body["ok"].as_bool() != Some(true) {
            bail!("Slack API error: {:?}", body["error"]);
        }

        info!(message_type = ?msg.message_type, "Sent Slack message");
        Ok(())
    }

    pub async fn fetch_new_messages(&self, last_ts: &Option<String>) -> Result<Vec<SlackMessage>> {
        let token = self.config.slack_bot_token.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Slack bot token not configured")
        })?;
        let channel = self.config.slack_channel_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Slack channel ID not configured")
        })?;

        let mut url = format!(
            "https://slack.com/api/conversations.history?channel={}&limit=50",
            channel
        );
        if let Some(ts) = last_ts {
            url.push_str(&format!("&oldest={}", ts));
        }

        let response = self
            .client
            .get(&url)
            .bearer_auth(token)
            .send()
            .await?;

        let body: SlackHistoryResponse = response.json().await?;
        if !body.ok {
            warn!(error = ?body.error, "Slack API error fetching messages");
            return Ok(vec![]);
        }

        let messages: Vec<SlackMessage> = body
            .messages
            .into_iter()
            .filter(|m| {
                if let Some(user) = &m.user {
                    user != &self.config.slack_bot_token.as_deref().unwrap_or("")
                } else {
                    true
                }
            })
            .collect();

        Ok(messages)
    }

    fn format_message(&self, msg: &NexusMessage) -> String {
        match msg.message_type {
            MessageType::WorkflowStarted => {
                format!(
                    "🚀 Starting ticket {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            MessageType::AgentAssigned => {
                format!(
                    "👷 {} assigned to {}",
                    msg.worker_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            MessageType::AgentCompleted => {
                format!(
                    "✅ {} completed: {}",
                    msg.worker_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            MessageType::WorkflowError => {
                format!(
                    "❌ {}: {}",
                    msg.worker_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            MessageType::QuestionToHuman => {
                format!("🤔 {}", msg.content)
            }
            MessageType::ApprovalRequest => {
                format!("⚠️ Approval needed: {}", msg.content)
            }
            MessageType::StatusUpdate => {
                format!("📊 {}", msg.content)
            }
            _ => msg.content.clone(),
        }
    }

    fn build_blocks(&self, msg: &NexusMessage) -> Vec<serde_json::Value> {
        match msg.message_type {
            MessageType::ApprovalRequest => {
                vec![json!({
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": format!("⚠️ *Approval Request*\n{}\n\n_Reply with `approve {}` to approve or `reject {}` to reject_",
                            msg.content,
                            msg.worker_id.as_deref().unwrap_or("?"),
                            msg.worker_id.as_deref().unwrap_or("?"))
                    }
                })]
            }
            MessageType::QuestionToHuman => {
                let options = msg.metadata.get("options")
                    .and_then(|o| o.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|o| o.as_str())
                            .enumerate()
                            .map(|(i, o)| format!("{}. {}", i + 1, o))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();

                vec![json!({
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": format!("🤔 *Question*\n{}\n\n{}\n\n_Reply with `answer {}: <your response>`_",
                            msg.content,
                            options,
                            msg.ticket_id.as_deref().unwrap_or("?"))
                    }
                })]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl ChatClient for SlackClient {
    async fn send_message(&self, msg: &NexusMessage) -> Result<()> {
        self.send_message(msg).await
    }

    async fn ask_human(
        &self,
        question: &str,
        options: &[&str],
        ticket_id: &str,
        _timeout_secs: u64,
    ) -> Option<String> {
        let msg = NexusMessage::question_to_human(ticket_id, question, options);
        if self.send_message(&msg).await.is_err() {
            return None;
        }
        None
    }

    async fn fetch_commands(&self, _channel_id: &str) -> Result<Vec<HumanCommand>> {
        Ok(vec![])
    }
}
