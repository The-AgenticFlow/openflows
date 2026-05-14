use anyhow::{bail, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::{ChatClient, ChatConfig, HumanCommand, MessageType, NexusMessage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiscordMessage {
    pub id: String,
    pub content: String,
    pub author_id: Option<String>,
    pub channel_id: String,
    pub message_reference: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct DiscordHistoryResponse {
    id: String,
    content: String,
    author: Option<DiscordAuthor>,
    channel_id: String,
    message_reference: Option<DiscordMessageReference>,
}

#[derive(Debug, Clone, Deserialize)]
struct DiscordAuthor {
    id: String,
    #[allow(dead_code)]
    username: String,
}

#[derive(Debug, Clone, Deserialize)]
struct DiscordMessageReference {
    message_id: Option<String>,
}

pub struct DiscordClient {
    client: Client,
    config: ChatConfig,
}

impl DiscordClient {
    pub fn new(config: ChatConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
    }

    pub async fn send_message(&self, msg: &NexusMessage) -> Result<()> {
        let token = self.config.discord_bot_token.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Discord bot token not configured")
        })?;
        let channel = self.config.discord_channel_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Discord channel ID not configured")
        })?;

        let text = self.format_message(msg);
        let embeds = self.build_embeds(msg);

        let response = self
            .client
            .post(&format!(
                "https://discord.com/api/v10/channels/{}/messages",
                channel
            ))
            .header("Authorization", format!("Bot {}", token))
            .header("Content-Type", "application/json")
            .json(&json!({
                "content": text,
                "embeds": embeds,
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let body: serde_json::Value = response.json().await?;
            bail!("Discord API error: {:?}", body);
        }

        info!(message_type = ?msg.message_type, "Sent Discord message");
        Ok(())
    }

    pub async fn fetch_new_messages(&self, last_ts: &Option<String>) -> Result<Vec<DiscordMessage>> {
        let token = self.config.discord_bot_token.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Discord bot token not configured")
        })?;
        let channel = self.config.discord_channel_id.as_ref().ok_or_else(|| {
            anyhow::anyhow!("Discord channel ID not configured")
        })?;

        let mut url = format!(
            "https://discord.com/api/v10/channels/{}/messages?limit=50",
            channel
        );
        if let Some(ts) = last_ts {
            url.push_str(&format!("&after={}", ts));
        }

        let response = self
            .client
            .get(&url)
            .header("Authorization", format!("Bot {}", token))
            .send()
            .await?;

        if !response.status().is_success() {
            warn!(status = ?response.status(), "Discord API error fetching messages");
            return Ok(vec![]);
        }

        let messages: Vec<DiscordHistoryResponse> = response.json().await?;
        let bot_id = self.config.discord_bot_token.as_deref().unwrap_or("");

        let messages: Vec<DiscordMessage> = messages
            .into_iter()
            .filter(|m| {
                if let Some(author) = &m.author {
                    author.id != bot_id
                } else {
                    true
                }
            })
            .map(|m| DiscordMessage {
                id: m.id,
                content: m.content,
                author_id: m.author.map(|a| a.id),
                channel_id: m.channel_id,
                message_reference: m.message_reference.and_then(|r| r.message_id),
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

    fn build_embeds(&self, msg: &NexusMessage) -> Vec<serde_json::Value> {
        match msg.message_type {
            MessageType::ApprovalRequest => {
                vec![json!({
                    "title": "Approval Request",
                    "description": msg.content,
                    "color": 16753920,
                    "footer": {
                        "text": format!("Reply with `approve {}` to approve or `reject {}` to reject",
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
                    "title": "Question",
                    "description": msg.content,
                    "color": 3447003,
                    "fields": [{
                        "name": "Options",
                        "value": options,
                        "inline": false
                    }],
                    "footer": {
                        "text": format!("Reply with `answer {}: <your response>`",
                            msg.ticket_id.as_deref().unwrap_or("?"))
                    }
                })]
            }
            _ => vec![],
        }
    }
}

#[async_trait]
impl ChatClient for DiscordClient {
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
