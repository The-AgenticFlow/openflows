use pocketflow_core::SharedStore;
use serde_json::json;
use tokio::sync::mpsc;
use tracing::{info, warn};

use crate::{ChannelType, ChatConfig, HumanMessage, run_discord_gateway};

pub const KEY_HUMAN_MESSAGES: &str = "human_messages";

/// Run the chat event loop.
/// 
/// This establishes event-driven connections to chat platforms:
/// - Discord: WebSocket Gateway (real-time MESSAGE_CREATE events)
/// - Slack/WhatsApp: Polling (fallback, if configured)
/// 
/// Messages are pushed to the SharedStore under KEY_HUMAN_MESSAGES.
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
        // In dev mode, just wait for shutdown
        let _ = shutdown.recv().await;
        info!("NEXUS chat loop shutting down");
        return;
    }

    // Channel for all incoming messages
    let (message_tx, mut message_rx) = mpsc::channel::<HumanMessage>(100);

    // Track spawned tasks for cleanup
    let mut task_handles: Vec<tokio::task::JoinHandle<()>> = Vec::new();
    // Track shutdown senders for graceful task termination
    let shutdown_senders: Vec<mpsc::Sender<()>> = Vec::new();

    // Shared shutdown signal for tasks that support watch-based shutdown (Discord Gateway)
    let (gw_shutdown_tx, gw_shutdown_rx) = tokio::sync::watch::channel(false);

    // Spawn Discord Gateway if configured
    let has_discord = active_channels.contains(&ChannelType::Discord);
    if has_discord {
        let config_clone = config.clone();
        let tx = message_tx.clone();
        let gw_rx = gw_shutdown_rx.clone();

        let handle = tokio::spawn(async move {
            if let Err(e) = run_discord_gateway(config_clone, tx, gw_rx).await {
                warn!("Discord Gateway error: {}", e);
            }
        });

        task_handles.push(handle);
        info!("Discord Gateway task spawned");
    }

    // Spawn Slack poller if configured (keep as backup for Slack users)
    let has_slack = active_channels.contains(&ChannelType::Slack);
    if has_slack {
        let tx = message_tx.clone();
        let config_clone = config.clone();
        let handle = tokio::spawn(async move {
            run_slack_poller(config_clone, tx).await;
        });
        task_handles.push(handle);
        info!("Slack poller task spawned");
    }

    // Spawn WhatsApp poller if configured
    let has_whatsapp = active_channels.contains(&ChannelType::WhatsApp);
    if has_whatsapp {
        let tx = message_tx.clone();
        let config_clone = config.clone();
        let handle = tokio::spawn(async move {
            run_whatsapp_poller(config_clone, tx).await;
        });
        task_handles.push(handle);
        info!("WhatsApp poller task spawned");
    }

    // Main message processor loop
    info!("NEXUS chat loop started - listening for human commands");
    loop {
        tokio::select! {
            _ = shutdown.recv() => {
                info!("NEXUS chat loop received shutdown signal");
                // Signal graceful shutdown to tasks that support watch-based shutdown
                let _ = gw_shutdown_tx.send(true);
                // Signal graceful shutdown to legacy mpsc-based tasks
                for tx in &shutdown_senders {
                    let _ = tx.send(()).await;
                }
                break;
            }

            msg = message_rx.recv() => {
                if let Some(human_msg) = msg {
                    // Store message for Nexus to process
                    let mut messages_list: Vec<HumanMessage> =
                        store.get_typed(KEY_HUMAN_MESSAGES).await.unwrap_or_default();
                    messages_list.push(human_msg);
                    store.set(KEY_HUMAN_MESSAGES, json!(messages_list)).await;
                }
            }
        }
    }

    // Allow a brief moment for graceful shutdowns to complete before aborting
    tokio::time::sleep(std::time::Duration::from_millis(500)).await;

    // Cleanup: abort all spawned tasks
    for handle in task_handles {
        handle.abort();
    }

    info!("NEXUS chat loop stopped");
}

/// Slack poller - polls for new messages every 2 seconds
/// This is a fallback for Slack since it doesn't have a simple WebSocket gateway
async fn run_slack_poller(
    config: ChatConfig,
    tx: mpsc::Sender<HumanMessage>,
) {
    use crate::SlackClient;

    let slack_client = match &config.slack_bot_token {
        Some(_token) if config.slack_channel_id.is_some() => {
            Some(SlackClient::new(config.clone()))
        }
        _ => None,
    };

    let mut last_ts: Option<String> = None;

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        if let Some(client) = &slack_client {
            match client.fetch_new_messages(&last_ts).await {
                Ok(messages) => {
                    for msg in messages {
                        let human_msg = HumanMessage::new(
                            msg.user.as_deref().unwrap_or("unknown"),
                            &msg.channel,
                            &msg.text,
                        );
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

/// WhatsApp poller - polls for new messages every 2 seconds
async fn run_whatsapp_poller(
    config: ChatConfig,
    tx: mpsc::Sender<HumanMessage>,
) {
    use crate::WhatsAppClient;

    let whatsapp_client = match (&config.whatsapp_api_key, &config.whatsapp_phone_number) {
        (Some(_), Some(_)) => Some(WhatsAppClient::new(config.clone())),
        _ => None,
    };

    let mut last_ts: Option<String> = None;

    loop {
        tokio::time::sleep(std::time::Duration::from_secs(2)).await;

        if let Some(client) = &whatsapp_client {
            match client.fetch_new_messages(&last_ts).await {
                Ok(messages) => {
                    for msg in messages {
                        let human_msg = HumanMessage::new(
                            &msg.from,
                            &msg.phone_number_id,
                            &msg.text,
                        );
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
