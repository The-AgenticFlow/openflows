use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::messages::{InboundMessage, OutboundMessage, OutboundMessageType};
use crate::plugin::ChannelPlugin;
use tokio::sync::{mpsc, watch};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppMessage {
    pub id: String,
    pub from: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub phone_number_id: String,
}

pub struct WhatsAppPlugin {
    client: Client,
    api_key: String,
    phone_number: String,
    api_url: String,
}

impl WhatsAppPlugin {
    pub fn new(api_key: String, phone_number: String, api_url: String) -> Self {
        Self {
            client: Client::new(),
            api_key,
            phone_number,
            api_url,
        }
    }

    pub fn from_config(config: &serde_json::Value) -> Option<Self> {
        let api_key = config.get("api_key")?.as_str()?;
        let phone = config.get("phone_number")?.as_str()?;
        let api_url = config.get("api_url")
            .and_then(|v| v.as_str())
            .unwrap_or("https://graph.facebook.com/v18.0")
            .to_string();
        Some(Self::new(api_key.to_string(), phone.to_string(), api_url))
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

    async fn send_to_whatsapp(&self, msg: &OutboundMessage) -> Result<()> {
        let text = self.format_message(msg);

        let response = self
            .client
            .post(format!("{}/{}/messages", self.api_url, self.phone_number))
            .bearer_auth(&self.api_key)
            .header("Content-Type", "application/json")
            .json(&json!({
                "messaging_product": "whatsapp",
                "to": self.phone_number,
                "type": "text",
                "text": {
                    "body": text
                }
            }))
            .send()
            .await?;

        if !response.status().is_success() {
            let body: serde_json::Value = response.json().await?;
            bail!("WhatsApp API error: {:?}", body);
        }

        info!(message_type = ?msg.message_type, "Sent WhatsApp message");
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
                                    message_id: msg.id.clone(),
                                    channel_id: "whatsapp".to_string(),
                                    user_id: msg.from.clone(),
                                    conversation_id: msg.phone_number_id.clone(),
                                    text: msg.text.clone(),
                                    timestamp: msg.timestamp,
                                    metadata: serde_json::Value::Null,
                                };
                                if let Err(e) = tx.send(human_msg).await {
                                    warn!("Failed to send WhatsApp message: {}", e);
                                }
                                last_ts = Some(msg.id);
                            }
                        }
                        Err(e) => {
                            warn!("Failed to fetch WhatsApp messages: {}", e);
                        }
                    }
                }
            }
        }
    }

    async fn fetch_messages(&self, last_ts: &Option<String>) -> Result<Vec<WhatsAppMessage>> {
        let mut url = format!(
            "{}/{}/messages?limit=50",
            self.api_url, self.phone_number
        );
        if let Some(ts) = last_ts {
            url.push_str(&format!("&after={}", ts));
        }

        let response = self
            .client
            .get(&url)
            .bearer_auth(&self.api_key)
            .send()
            .await?;

        if !response.status().is_success() {
            warn!(status = ?response.status(), "WhatsApp API error fetching messages");
            return Ok(vec![]);
        }

        let body: serde_json::Value = response.json().await?;
        let messages = body
            .get("data")
            .and_then(|d| d.as_array())
            .cloned()
            .unwrap_or_default();

        let result: Vec<WhatsAppMessage> = messages
            .into_iter()
            .filter_map(|m| {
                let from = m.get("from").and_then(|f| f.as_str())?.to_string();
                let id = m.get("id").and_then(|f| f.as_str())?.to_string();
                let text = m.get("text")
                    .and_then(|t| t.get("body"))
                    .and_then(|b| b.as_str())?
                    .to_string();
                let timestamp = m.get("timestamp")
                    .and_then(|t| t.as_str())
                    .and_then(|t| t.parse::<i64>().ok())
                    .map(|t| DateTime::from_timestamp(t, 0).unwrap_or_else(Utc::now))
                    .unwrap_or_else(Utc::now);
                let phone_number_id = m.get("messaging_product")
                    .and_then(|p| p.as_str())
                    .unwrap_or(&self.phone_number)
                    .to_string();

                Some(WhatsAppMessage {
                    id,
                    from,
                    text,
                    timestamp,
                    phone_number_id,
                })
            })
            .collect();

        Ok(result)
    }
}

#[async_trait]
impl ChannelPlugin for WhatsAppPlugin {
    fn channel_id(&self) -> &str {
        "whatsapp"
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        self.poll_messages(tx, shutdown).await
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        self.send_to_whatsapp(msg).await
    }

    async fn ask_human(
        &self,
        question: &str,
        _options: &[&str],
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
            metadata: serde_json::Value::Null,
        };
        if self.send_to_whatsapp(&msg).await.is_err() {
            return None;
        }
        None
    }
}
