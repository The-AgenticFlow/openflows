mod chat_loop;
mod discord_client;
mod discord_gateway;
mod message_types;
mod mock;
mod rate_limit;
mod slack_client;
mod whatsapp_client;

pub use chat_loop::{run_chat_loop, KEY_HUMAN_MESSAGES};
pub use discord_client::{DiscordClient, DiscordMessage};
pub use discord_gateway::run_discord_gateway;
pub use message_types::{ChannelType, ChatConfig, HumanCommand, HumanMessage, MessageType, NexusMessage};
pub use mock::MockChatClient;
pub use rate_limit::RateLimiter;
pub use slack_client::{SlackClient, SlackMessage};
pub use whatsapp_client::{WhatsAppClient, WhatsAppMessage};

use anyhow::Result;
use async_trait::async_trait;
use pocketflow_core::SharedStore;

#[async_trait]
pub trait ChatClient: Send + Sync {
    async fn send_message(&self, msg: &NexusMessage) -> Result<()>;
    async fn ask_human(
        &self,
        question: &str,
        options: &[&str],
        ticket_id: &str,
        timeout_secs: u64,
    ) -> Option<String>;
    async fn fetch_commands(&self, channel_id: &str) -> Result<Vec<HumanCommand>>;
}

pub struct HumanChannel {
    store: SharedStore,
    clients: Vec<(ChannelType, Box<dyn ChatClient>)>,
    config: ChatConfig,
}

impl HumanChannel {
    pub fn new(store: SharedStore, config: ChatConfig) -> Self {
        let mut clients: Vec<(ChannelType, Box<dyn ChatClient>)> = Vec::new();

        if config.dev_mode {
            clients.push((ChannelType::Slack, Box::new(MockChatClient::new())));
        } else {
            if config.slack_bot_token.is_some() && config.slack_channel_id.is_some() {
                clients.push((ChannelType::Slack, Box::new(SlackClient::new(config.clone()))));
            }
            if config.discord_bot_token.is_some() && config.discord_channel_id.is_some() {
                clients.push((ChannelType::Discord, Box::new(DiscordClient::new(config.clone()))));
            }
            if config.whatsapp_api_key.is_some() && config.whatsapp_phone_number.is_some() {
                clients.push((ChannelType::WhatsApp, Box::new(WhatsAppClient::new(config.clone()))));
            }
            if clients.is_empty() {
                clients.push((ChannelType::Slack, Box::new(MockChatClient::new())));
            }
        }

        Self { store, clients, config }
    }

    pub async fn notify(&self, msg: NexusMessage) -> Result<()> {
        let mut last_error = None;
        for (_channel_type, client) in &self.clients {
            if let Err(e) = client.send_message(&msg).await {
                last_error = Some(e);
            }
        }

        self.store
            .emit(
                "nexus_chat",
                "message_sent",
                serde_json::to_value(&msg)?,
            )
            .await;

        if let Some(e) = last_error {
            Err(e)
        } else {
            Ok(())
        }
    }

    pub async fn notify_to_channel(&self, channel_type: ChannelType, msg: NexusMessage) -> Result<()> {
        for (ctype, client) in &self.clients {
            if *ctype == channel_type {
                client.send_message(&msg).await?;
                self.store
                    .emit(
                        "nexus_chat",
                        "message_sent",
                        serde_json::to_value(&msg)?,
                    )
                    .await;
                return Ok(());
            }
        }
        Err(anyhow::anyhow!("Channel type {:?} not configured", channel_type))
    }

    pub async fn ask_human(
        &self,
        question: &str,
        options: &[&str],
        ticket_id: &str,
        timeout_secs: u64,
    ) -> Option<String> {
        for (_channel_type, client) in &self.clients {
            if let Some(response) = client.ask_human(question, options, ticket_id, timeout_secs).await {
                return Some(response);
            }
        }
        None
    }

    pub async fn pending_messages(&self) -> Vec<HumanMessage> {
        self.store
            .get_typed(KEY_HUMAN_MESSAGES)
            .await
            .unwrap_or_default()
    }

    pub async fn ack_message(&self, msg: &HumanMessage) {
        let mut messages: Vec<HumanMessage> = self
            .store
            .get_typed(KEY_HUMAN_MESSAGES)
            .await
            .unwrap_or_default();
        // Compare by deterministic fields (not timestamp, which may drift through JSON
        // round-trips with nanosecond precision).
        messages.retain(|m| {
            !(m.user_id == msg.user_id && m.channel_id == msg.channel_id && m.text == msg.text)
        });
        self.store
            .set(KEY_HUMAN_MESSAGES, serde_json::json!(messages))
            .await;
    }

    pub async fn inject_message(&self, msg: HumanMessage) -> Result<()> {
        let mut messages: Vec<HumanMessage> = self
            .store
            .get_typed(KEY_HUMAN_MESSAGES)
            .await
            .unwrap_or_default();
        messages.push(msg);
        self.store
            .set(KEY_HUMAN_MESSAGES, serde_json::json!(messages))
            .await;
        Ok(())
    }

    pub fn is_enabled(&self) -> bool {
        self.config.enabled
    }

    pub fn is_dev_mode(&self) -> bool {
        self.config.dev_mode
    }

    pub fn active_channels(&self) -> Vec<ChannelType> {
        self.clients.iter().map(|(ctype, _)| ctype.clone()).collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_human_channel_notify() {
        let store = SharedStore::new_in_memory();
        let config = ChatConfig {
            enabled: true,
            dev_mode: true,
            ..Default::default()
        };
        let channel = HumanChannel::new(store.clone(), config);

        let msg = NexusMessage::status_update("Test notification");
        let result = channel.notify(msg).await;
        assert!(result.is_ok());
    }

    #[tokio::test]
    async fn test_human_channel_pending_messages() {
        let store = SharedStore::new_in_memory();
        let config = ChatConfig {
            enabled: true,
            dev_mode: true,
            ..Default::default()
        };
        let channel = HumanChannel::new(store.clone(), config);

        let msg = HumanMessage::new("U123", "C123", "pause T-001");
        channel.inject_message(msg.clone()).await.unwrap();

        let pending = channel.pending_messages().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].text, "pause T-001");
    }

    #[tokio::test]
    async fn test_human_channel_ack_message() {
        let store = SharedStore::new_in_memory();
        let config = ChatConfig {
            enabled: true,
            dev_mode: true,
            ..Default::default()
        };
        let channel = HumanChannel::new(store.clone(), config);

        let msg = HumanMessage::new("U123", "C123", "pause T-001");
        channel.inject_message(msg.clone()).await.unwrap();

        channel.ack_message(&msg).await;

        let pending = channel.pending_messages().await;
        assert!(pending.is_empty());
    }
}
