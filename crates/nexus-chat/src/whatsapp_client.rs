use anyhow::{bail, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use tracing::{info, warn};

use crate::{ChatClient, ChatConfig, HumanCommand, MessageType, NexusMessage};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WhatsAppMessage {
    pub id: String,
    pub from: String,
    pub text: String,
    pub timestamp: DateTime<Utc>,
    pub phone_number_id: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct WhatsAppIncomingMessage {
    from: String,
    id: String,
    timestamp: String,
    text: Option<WhatsAppText>,
    r#type: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct WhatsAppText {
    body: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct WhatsAppWebhookEntry {
    changes: Vec<WhatsAppChange>,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct WhatsAppChange {
    value: WhatsAppChangeValue,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct WhatsAppChangeValue {
    contacts: Option<Vec<WhatsAppContact>>,
    messages: Option<Vec<WhatsAppIncomingMessage>>,
    metadata: WhatsAppMetadata,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct WhatsAppContact {
    wa_id: String,
}

#[allow(dead_code)]
#[derive(Debug, Clone, Deserialize)]
struct WhatsAppMetadata {
    phone_number_id: String,
    display_phone_number: String,
}

pub struct WhatsAppClient {
    client: Client,
    config: ChatConfig,
}

impl WhatsAppClient {
    pub fn new(config: ChatConfig) -> Self {
        Self {
            client: Client::new(),
            config,
        }
    }

    pub async fn send_message(&self, msg: &NexusMessage) -> Result<()> {
        let api_key = self.config.whatsapp_api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("WhatsApp API key not configured")
        })?;
        let phone_number = self.config.whatsapp_phone_number.as_ref().ok_or_else(|| {
            anyhow::anyhow!("WhatsApp phone number not configured")
        })?;
        let api_url = self.config.whatsapp_api_url.as_deref().unwrap_or("https://graph.facebook.com/v18.0");

        let text = self.format_message(msg);

        let response = self
            .client
            .post(&format!(
                "{}/{}/messages",
                api_url, phone_number
            ))
            .bearer_auth(api_key)
            .header("Content-Type", "application/json")
            .json(&json!({
                "messaging_product": "whatsapp",
                "to": phone_number,
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

    pub async fn fetch_new_messages(&self, last_ts: &Option<String>) -> Result<Vec<WhatsAppMessage>> {
        let api_key = self.config.whatsapp_api_key.as_ref().ok_or_else(|| {
            anyhow::anyhow!("WhatsApp API key not configured")
        })?;
        let phone_number = self.config.whatsapp_phone_number.as_ref().ok_or_else(|| {
            anyhow::anyhow!("WhatsApp phone number not configured")
        })?;
        let api_url = self.config.whatsapp_api_url.as_deref().unwrap_or("https://graph.facebook.com/v18.0");

        let mut url = format!(
            "{}/{}/messages?limit=50",
            api_url, phone_number
        );
        if let Some(ts) = last_ts {
            url.push_str(&format!("&after={}", ts));
        }

        let response = self
            .client
            .get(&url)
            .bearer_auth(api_key)
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
                    .unwrap_or(phone_number)
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

    pub fn handle_webhook(&self, payload: &serde_json::Value) -> Result<Vec<WhatsAppMessage>> {
        let entry = payload
            .get("entry")
            .and_then(|e| e.as_array())
            .ok_or_else(|| anyhow::anyhow!("Invalid webhook payload: missing entry"))?;

        let mut messages = Vec::new();

        for e in entry {
            let changes = e
                .get("changes")
                .and_then(|c| c.as_array())
                .cloned()
                .unwrap_or_default();

            for change in changes {
                let value = change.get("value").cloned().unwrap_or_default();
                let metadata = value.get("metadata").cloned().unwrap_or_default();
                let phone_number_id = metadata
                    .get("phone_number_id")
                    .and_then(|p| p.as_str())
                    .unwrap_or("")
                    .to_string();

                if let Some(msgs) = value.get("messages").and_then(|m| m.as_array()) {
                    for msg in msgs {
                        if let Some(text_obj) = msg.get("text") {
                            let text = text_obj
                                .get("body")
                                .and_then(|b| b.as_str())
                                .unwrap_or("")
                                .to_string();
                            let from = msg
                                .get("from")
                                .and_then(|f| f.as_str())
                                .unwrap_or("")
                                .to_string();
                            let id = msg
                                .get("id")
                                .and_then(|f| f.as_str())
                                .unwrap_or("")
                                .to_string();
                            let timestamp = msg
                                .get("timestamp")
                                .and_then(|t| t.as_str())
                                .and_then(|t| t.parse::<i64>().ok())
                                .map(|t| DateTime::from_timestamp(t, 0).unwrap_or_else(Utc::now))
                                .unwrap_or_else(Utc::now);

                            messages.push(WhatsAppMessage {
                                id,
                                from,
                                text,
                                timestamp,
                                phone_number_id: phone_number_id.clone(),
                            });
                        }
                    }
                }
            }
        }

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
}

#[async_trait]
impl ChatClient for WhatsAppClient {
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
