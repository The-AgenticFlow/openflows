use pocketflow_core::SharedStore;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{ChannelType, ChatConfig, HumanMessage, SlackClient, DiscordClient, WhatsAppClient};

pub const KEY_HUMAN_MESSAGES: &str = "human_messages";

pub async fn run_chat_loop(
    store: SharedStore,
    config: ChatConfig,
    mut shutdown: mpsc::Receiver<()>,
) {
    if !config.enabled {
        info!("NEXUS chat disabled, not starting loop");
        return;
    }

    let active_channels = config.active_channels();
    info!("NEXUS chat starting with channels: {:?}", active_channels);

    if config.dev_mode {
        info!("NEXUS chat running in dev mode (mock client)");
    }

    let slack_client = if active_channels.contains(&ChannelType::Slack) && !config.dev_mode {
        Some(SlackClient::new(config.clone()))
    } else {
        None
    };

    let discord_client = if active_channels.contains(&ChannelType::Discord) && !config.dev_mode {
        Some(DiscordClient::new(config.clone()))
    } else {
        None
    };

    let whatsapp_client = if active_channels.contains(&ChannelType::WhatsApp) && !config.dev_mode {
        Some(WhatsAppClient::new(config.clone()))
    } else {
        None
    };

    let mut slack_last_ts: Option<String> = None;
    let mut discord_last_ts: Option<String> = None;
    let mut whatsapp_last_ts: Option<String> = None;

    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!("NEXUS chat loop received shutdown signal");
                break;
            }
            _ = tokio::time::sleep(std::time::Duration::from_secs(2)) => {
                if config.dev_mode {
                    continue;
                }

                if let Some(client) = &slack_client {
                    match client.fetch_new_messages(&slack_last_ts).await {
                        Ok(messages) => {
                            for msg in messages {
                                let human_msg = HumanMessage::new(
                                    msg.user.as_deref().unwrap_or("unknown"),
                                    &msg.channel,
                                    &msg.text,
                                );
                                let mut messages_list: Vec<HumanMessage> =
                                    store.get_typed(KEY_HUMAN_MESSAGES).await.unwrap_or_default();
                                messages_list.push(human_msg);
                                store.set(KEY_HUMAN_MESSAGES, json!(messages_list)).await;
                                slack_last_ts = Some(msg.ts);
                            }
                        }
                        Err(e) => {
                            warn!("Failed to fetch Slack messages: {}", e);
                        }
                    }
                }

                if let Some(client) = &discord_client {
                    match client.fetch_new_messages(&discord_last_ts).await {
                        Ok(messages) => {
                            for msg in messages {
                                let human_msg = HumanMessage::new(
                                    msg.author_id.as_deref().unwrap_or("unknown"),
                                    &msg.channel_id,
                                    &msg.content,
                                );
                                let mut messages_list: Vec<HumanMessage> =
                                    store.get_typed(KEY_HUMAN_MESSAGES).await.unwrap_or_default();
                                messages_list.push(human_msg);
                                store.set(KEY_HUMAN_MESSAGES, json!(messages_list)).await;
                                discord_last_ts = Some(msg.id);
                            }
                        }
                        Err(e) => {
                            warn!("Failed to fetch Discord messages: {}", e);
                        }
                    }
                }

                if let Some(client) = &whatsapp_client {
                    match client.fetch_new_messages(&whatsapp_last_ts).await {
                        Ok(messages) => {
                            for msg in messages {
                                let human_msg = HumanMessage::new(
                                    &msg.from,
                                    &msg.phone_number_id,
                                    &msg.text,
                                );
                                let mut messages_list: Vec<HumanMessage> =
                                    store.get_typed(KEY_HUMAN_MESSAGES).await.unwrap_or_default();
                                messages_list.push(human_msg);
                                store.set(KEY_HUMAN_MESSAGES, json!(messages_list)).await;
                                whatsapp_last_ts = Some(msg.id);
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
}

#[cfg(test)]
mod tests {
    use crate::{DiscordMessage, SlackMessage, WhatsAppMessage};

    fn make_slack_message(text: &str) -> SlackMessage {
        SlackMessage {
            ts: "1234567890.123456".to_string(),
            text: text.to_string(),
            user: Some("U123".to_string()),
            channel: "C123".to_string(),
            thread_ts: None,
        }
    }

    fn make_discord_message(content: &str) -> DiscordMessage {
        DiscordMessage {
            id: "987654321".to_string(),
            content: content.to_string(),
            author_id: Some("D123".to_string()),
            channel_id: "DC123".to_string(),
            message_reference: None,
        }
    }

    fn make_whatsapp_message(text: &str) -> WhatsAppMessage {
        use chrono::Utc;
        WhatsAppMessage {
            id: "wamid.123".to_string(),
            from: "+1234567890".to_string(),
            text: text.to_string(),
            timestamp: Utc::now(),
            phone_number_id: "WA123".to_string(),
        }
    }

    #[test]
    fn test_slack_message_creation() {
        let msg = make_slack_message("hello world");
        assert_eq!(msg.text, "hello world");
        assert_eq!(msg.user, Some("U123".to_string()));
        assert_eq!(msg.channel, "C123".to_string());
    }

    #[test]
    fn test_discord_message_creation() {
        let msg = make_discord_message("hello discord");
        assert_eq!(msg.content, "hello discord");
        assert_eq!(msg.author_id, Some("D123".to_string()));
        assert_eq!(msg.channel_id, "DC123".to_string());
    }

    #[test]
    fn test_whatsapp_message_creation() {
        let msg = make_whatsapp_message("hello whatsapp");
        assert_eq!(msg.text, "hello whatsapp");
        assert_eq!(msg.from, "+1234567890");
        assert_eq!(msg.phone_number_id, "WA123");
    }
}
