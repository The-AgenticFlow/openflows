use anyhow::{bail, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::messages::{InboundMessage, OutboundMessage, OutboundMessageType};
use crate::plugin::ChannelPlugin;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SlackMessage {
    ts: String,
    text: String,
    user: Option<String>,
    channel: String,
    thread_ts: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct SlackHistoryResponse {
    ok: bool,
    messages: Vec<SlackMessage>,
    error: Option<String>,
}

pub struct SlackPlugin {
    client: Client,
    bot_token: String,
    channel_id: String,
}

impl SlackPlugin {
    pub fn new(bot_token: String, channel_id: String) -> Self {
        Self {
            client: Client::new(),
            bot_token,
            channel_id,
        }
    }

    pub fn from_config(config: &serde_json::Value) -> Option<Self> {
        let token = config.get("bot_token")?.as_str()?;
        let channel = config.get("channel_id")?.as_str()?;
        Some(Self::new(token.to_string(), channel.to_string()))
    }

    fn format_message(&self, msg: &OutboundMessage) -> String {
        match msg.message_type {
            OutboundMessageType::WorkflowStarted => {
                format!(
                    "🚀 Starting ticket {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::AgentAssigned => {
                format!(
                    "👷 {} assigned to {}",
                    msg.worker_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::AgentCompleted => {
                format!(
                    "✅ {} completed: {}",
                    msg.worker_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::WorkflowError => {
                format!(
                    "❌ {}: {}",
                    msg.worker_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::QuestionToHuman => {
                format!("🤔 {}", msg.content)
            }
            OutboundMessageType::ApprovalRequest => {
                format!("⚠️ Approval needed: {}", msg.content)
            }
            OutboundMessageType::StatusUpdate => {
                format!("📊 {}", msg.content)
            }
            _ => msg.content.clone(),
        }
    }

    fn build_blocks(&self, msg: &OutboundMessage) -> Vec<serde_json::Value> {
        match msg.message_type {
            OutboundMessageType::ApprovalRequest => {
                vec![json!({
                    "type": "section",
                    "text": {
                        "type": "mrkdwn",
                        "text": format!(
                            "⚠️ *Approval Request*\n{}\n\n_Reply with `approve {}` to approve or `reject {}` to reject_",
                            msg.content,
                            msg.worker_id.as_deref().unwrap_or("?"),
                            msg.worker_id.as_deref().unwrap_or("?")
                        )
                    }
                })]
            }
            OutboundMessageType::QuestionToHuman => {
                let options = msg
                    .metadata
                    .get("options")
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
                        "text": format!(
                            "🤔 *Question*\n{}\n\n{}\n\n_Reply with `answer {}: <your response>`_",
                            msg.content,
                            options,
                            msg.ticket_id.as_deref().unwrap_or("?")
                        )
                    }
                })]
            }
            _ => vec![],
        }
    }

    async fn send_to_slack(&self, msg: &OutboundMessage) -> Result<()> {
        let text = self.format_message(msg);
        let blocks = self.build_blocks(msg);

        let response = self
            .client
            .post("https://slack.com/api/chat.postMessage")
            .bearer_auth(&self.bot_token)
            .json(&json!({
                "channel": &self.channel_id,
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

    async fn poll_messages(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        let mut last_ts: Option<String> = None;

        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break Ok(());
                    }
                }
                _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                    match self.fetch_messages(&last_ts).await {
                        Ok(messages) => {
                            for msg in messages {
                                let human_msg = InboundMessage {
                                    message_id: msg.ts.clone(),
                                    channel_id: "slack".to_string(),
                                    user_id: msg.user.as_deref().unwrap_or("unknown").to_string(),
                                    conversation_id: msg.channel.clone(),
                                    text: msg.text.clone(),
                                    timestamp: chrono::Utc::now(),
                                    metadata: json!({
                                        "thread_ts": msg.thread_ts,
                                    }),
                                };
                                if let Err(e) = tx.send(human_msg).await {
                                    warn!("Failed to send Slack message: {}", e);
                                }
                                last_ts = Some(msg.ts);
                            }
                        }
                        Err(e) => {
                            warn!("Failed to fetch Slack messages: {}", e);
                        }
                    }
                }
            }
        }
    }

    async fn fetch_messages(&self, last_ts: &Option<String>) -> Result<Vec<SlackMessage>> {
        let mut url = format!(
            "https://slack.com/api/conversations.history?channel={}&limit=50",
            self.channel_id
        );
        if let Some(ts) = last_ts {
            url.push_str(&format!("&oldest={}", ts));
        }

        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.bot_token)
            .send()
            .await?;

        let body: SlackHistoryResponse = response.json().await?;
        if !body.ok {
            warn!(error = ?body.error, "Slack API error fetching messages");
            return Ok(vec![]);
        }

        // Filter out bot's own messages
        let bot_user = self.bot_token.clone();
        let messages: Vec<SlackMessage> = body
            .messages
            .into_iter()
            .filter(|m| {
                if let Some(user) = &m.user {
                    user != &bot_user
                } else {
                    true
                }
            })
            .collect();

        Ok(messages)
    }
}

#[async_trait]
impl ChannelPlugin for SlackPlugin {
    fn channel_id(&self) -> &str {
        "slack"
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        self.poll_messages(tx, shutdown).await
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        self.send_to_slack(msg).await
    }

    async fn ask_human(
        &self,
        question: &str,
        options: &[&str],
        ticket_id: &str,
        _timeout_secs: u64,
    ) -> Option<String> {
        let msg = OutboundMessage {
            message_type: OutboundMessageType::QuestionToHuman,
            target_channel: None,
            target_conversation: None,
            content: question.to_string(),
            ticket_id: Some(ticket_id.to_string()),
            worker_id: None,
            metadata: json!({"options": options}),
        };
        if self.send_to_slack(&msg).await.is_err() {
            return None;
        }
        // Slack async question doesn't wait for response in this implementation
        None
    }
}
