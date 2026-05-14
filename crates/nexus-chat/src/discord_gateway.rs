use anyhow::Result;
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::mpsc;
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};
use tracing::{info, warn, debug};

use crate::{ChatConfig, HumanMessage};

// Discord Gateway intents
// GUILD_MESSAGES = 1 << 9 = 512
// MESSAGE_CONTENT = 1 << 15 = 32768
const INTENTS: u64 = 512 | 32768;

const GATEWAY_URL: &str = "wss://gateway.discord.gg/?v=10&encoding=json";

#[derive(Debug, Clone, Deserialize)]
struct GatewayPayload {
    op: u8,
    #[serde(rename = "d")]
    data: Option<serde_json::Value>,
    #[serde(rename = "s")]
    sequence: Option<u64>,
    #[serde(rename = "t")]
    event_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct HelloData {
    heartbeat_interval: u64,
}

#[derive(Debug, Clone, Deserialize)]
struct ReadyData {
    user: GatewayUser,
}

#[derive(Debug, Clone, Deserialize)]
struct GatewayUser {
    id: String,
    username: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MessageCreateAuthor {
    id: String,
    username: String,
    #[serde(default)]
    bot: bool,
}

#[derive(Debug, Clone, Deserialize)]
struct MentionUser {
    id: String,
}

#[derive(Debug, Clone, Deserialize)]
struct MessageCreateData {
    #[allow(dead_code)]
    id: String,
    content: String,
    channel_id: String,
    author: MessageCreateAuthor,
    #[serde(default)]
    mentions: Vec<MentionUser>,
}

/// Run Discord Gateway connection with automatic reconnection.
///
/// This establishes a WebSocket connection to Discord and receives MESSAGE_CREATE events
/// in real-time, filtering for messages that @mention the bot or start with the bot's username
/// (case-insensitive). If the connection drops, it reconnects with exponential backoff.
pub async fn run_discord_gateway(
    config: ChatConfig,
    message_tx: mpsc::Sender<HumanMessage>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    info!("Discord Gateway background task starting");
    let mut backoff_secs = 1u64;

    loop {
        let shutdown_val = *shutdown_rx.borrow();
        if shutdown_val {
            info!("Discord Gateway shutdown requested, exiting reconnect loop");
            break Ok(());
        }

        info!("Discord Gateway connecting (attempt)");
        match run_gateway_once(config.clone(), message_tx.clone(), shutdown_rx.clone()).await {
            Ok(()) => {
                if *shutdown_rx.borrow() {
                    info!("Discord Gateway shut down gracefully");
                    break Ok(());
                }
                // Server closed the connection — reconnect
                warn!("Discord Gateway connection closed, will reconnect");
            }
            Err(e) => {
                warn!("Discord Gateway connection error: {}", e);
                if *shutdown_rx.borrow() {
                    break Ok(());
                }
            }
        }

        warn!(
            seconds = backoff_secs,
            "Discord Gateway reconnecting with backoff"
        );
        tokio::select! {
            _ = tokio::time::sleep(std::time::Duration::from_secs(backoff_secs)) => {}
            _ = shutdown_rx.changed() => {
                info!("Discord Gateway shutdown requested during backoff, exiting");
                break Ok(());
            }
        }
        backoff_secs = (backoff_secs * 2).min(60);
    }
}

async fn run_gateway_once(
    config: ChatConfig,
    message_tx: mpsc::Sender<HumanMessage>,
    mut shutdown_rx: tokio::sync::watch::Receiver<bool>,
) -> Result<()> {
    let token = config.discord_bot_token.as_ref().ok_or_else(|| {
        anyhow::anyhow!("Discord bot token not configured")
    })?;
    let target_channel = config.discord_channel_id.as_ref().ok_or_else(|| {
        anyhow::anyhow!("Discord channel ID not configured")
    })?;

    info!("run_gateway_once: connecting to Discord Gateway");

    let (ws_stream, _) = connect_async(GATEWAY_URL).await?;
    let (mut ws_sink, mut ws_stream) = ws_stream.split();
    info!("run_gateway_once: WebSocket connected");

    let mut heartbeat_interval: u64 = 41250;
    let mut next_heartbeat = tokio::time::Instant::now()
        + std::time::Duration::from_millis(heartbeat_interval);
    let mut sequence: Option<u64> = None;
    let mut bot_user_id: Option<String> = None;
    let mut bot_username: Option<String> = None;

    // State machine for connection
    enum ConnectionState {
        WaitingForHello,
        WaitingForReady,
        Connected,
    }
    let mut state = ConnectionState::WaitingForHello;

    loop {
        let now = tokio::time::Instant::now();
        let sleep_duration = next_heartbeat.saturating_duration_since(now);

        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("Discord Gateway received shutdown signal");
                let _ = ws_sink.close().await;
                return Ok(());
            }

            msg = ws_stream.next() => {
                match msg {
                    Some(Ok(WsMessage::Text(text))) => {
                        let payload: GatewayPayload = match serde_json::from_str(&text) {
                            Ok(p) => p,
                            Err(e) => {
                                warn!("Failed to parse gateway payload: {}", e);
                                continue;
                            }
                        };

                        debug!(op = payload.op, event = ?payload.event_type, "Gateway message received");

                        // Update sequence number
                        if payload.sequence.is_some() {
                            sequence = payload.sequence;
                        }

                        match payload.op {
                            // Heartbeat Request — Discord asks us to send a heartbeat NOW
                            1 => {
                                info!("Discord Gateway requested immediate heartbeat");
                                let heartbeat = json!({
                                    "op": 1,
                                    "d": sequence
                                });
                                if let Err(e) = ws_sink.send(WsMessage::Text(heartbeat.to_string())).await {
                                    warn!("Failed to send requested heartbeat: {}", e);
                                } else {
                                    // Reset the scheduled heartbeat timer
                                    next_heartbeat = tokio::time::Instant::now()
                                        + std::time::Duration::from_millis(heartbeat_interval);
                                    info!("Immediate heartbeat sent to Discord Gateway");
                                }
                            }

                            // Hello - contains heartbeat interval
                            10 => {
                                if let Some(data) = payload.data {
                                    if let Ok(hello) = serde_json::from_value::<HelloData>(data) {
                                        heartbeat_interval = hello.heartbeat_interval;
                                        // Discord recommends sending first heartbeat after a random jitter
                                        // to avoid thundering herd. We use a simple fixed half-interval.
                                        next_heartbeat = tokio::time::Instant::now()
                                            + std::time::Duration::from_millis(heartbeat_interval / 2);
                                        info!(
                                            interval = heartbeat_interval,
                                            "Discord Gateway Hello received"
                                        );

                                        // Send Identify
                                        let identify = json!({
                                            "op": 2,
                                            "d": {
                                                "token": token,
                                                "intents": INTENTS,
                                                "properties": {
                                                    "os": "linux",
                                                    "browser": "nexus-chat",
                                                    "device": "nexus-chat"
                                                }
                                            }
                                        });

                                        ws_sink.send(WsMessage::Text(identify.to_string())).await?;
                                        info!("Sent Identify to Discord Gateway");
                                        state = ConnectionState::WaitingForReady;
                                    }
                                }
                            }

                            // Heartbeat ACK
                            11 => {
                                debug!("Heartbeat ACK received");
                            }

                            // Event dispatch
                            0 => {
                                if let Some(event_type) = &payload.event_type {
                                    match event_type.as_str() {
                                        "READY" => {
                                            if let Some(data) = payload.data.clone() {
                                                if let Ok(ready) = serde_json::from_value::<ReadyData>(data) {
                                                    bot_user_id = Some(ready.user.id.clone());
                                                    bot_username = Some(ready.user.username.clone());
                                                    info!(
                                                        bot_id = %ready.user.id,
                                                        bot_name = %ready.user.username,
                                                        "Discord Gateway READY - connected as bot"
                                                    );
                                                    state = ConnectionState::Connected;
                                                } else {
                                                    warn!("Discord Gateway READY event payload failed to deserialize");
                                                }
                                            } else {
                                                warn!("Discord Gateway READY event missing payload data");
                                            }
                                        }

                                        "MESSAGE_CREATE" => {
                                            if let Some(data) = payload.data.clone() {
                                                if let Ok(msg_data) = serde_json::from_value::<MessageCreateData>(data) {
                                                    info!(
                                                        author = %msg_data.author.username,
                                                        channel = %msg_data.channel_id,
                                                        content = %msg_data.content,
                                                        mentions_count = msg_data.mentions.len(),
                                                        is_bot = msg_data.author.bot,
                                                        "MESSAGE_CREATE received"
                                                    );
                                                    // Filter for target channel
                                                    if &msg_data.channel_id == target_channel {
                                                        // Ignore bot's own messages
                                                        if !msg_data.author.bot {
                                                            // Check if message @mentions the bot or starts with bot username (case-insensitive)
                                                            let is_mentioned = bot_user_id.as_ref().map(|bot_id| {
                                                                let matched = msg_data.mentions.iter().any(|m| &m.id == bot_id);
                                                                info!(bot_id, matched, "Mention check");
                                                                matched
                                                            }).unwrap_or(false);

                                                            let starts_with_bot = bot_username.as_ref().map(|name| {
                                                                let lower_content = msg_data.content.to_lowercase();
                                                                let lower_name = name.to_lowercase();
                                                                let starts = lower_content.starts_with(&lower_name)
                                                                    && (lower_content.len() == lower_name.len()
                                                                        || lower_content[lower_name.len()..].starts_with(char::is_whitespace));
                                                                info!(name, starts, content = %msg_data.content, "Bot username prefix check");
                                                                starts
                                                            }).unwrap_or(false);

                                                            if is_mentioned || starts_with_bot {
                                                                // Strip the trigger from content
                                                                let content = if starts_with_bot {
                                                                    strip_prefix(&msg_data.content, bot_username.as_ref().unwrap())
                                                                } else if is_mentioned {
                                                                    strip_mention(&msg_data.content, bot_user_id.as_ref().unwrap())
                                                                } else {
                                                                    msg_data.content.clone()
                                                                };

                                                                let human_msg = HumanMessage::new(
                                                                    &msg_data.author.id,
                                                                    &msg_data.channel_id,
                                                                    &content,
                                                                );
                                                                info!(
                                                                    author = %msg_data.author.username,
                                                                    content = %content,
                                                                    "Discord command received"
                                                                );
                                                                if let Err(e) = message_tx.send(human_msg).await {
                                                                    warn!("Failed to send message to channel: {}", e);
                                                                }
                                                            } else {
                                                                info!("MESSAGE_CREATE ignored: not a mention and does not start with bot username");
                                                            }
                                                        } else {
                                                            debug!("MESSAGE_CREATE ignored: bot's own message");
                                                        }
                                                    } else {
                                                        debug!(msg_channel = %msg_data.channel_id, target_channel, "MESSAGE_CREATE ignored: wrong channel");
                                                    }
                                                } else {
                                                    warn!("Failed to deserialize MESSAGE_CREATE payload");
                                                }
                                            } else {
                                                warn!("MESSAGE_CREATE event missing payload data");
                                            }
                                        }

                                        "GUILD_CREATE" => {
                                            debug!("GUILD_CREATE event received");
                                        }

                                        _ => {
                                            debug!(event = %event_type, "Unhandled gateway event");
                                        }
                                    }
                                }
                            }

                            // Invalid session
                            9 => {
                                warn!("Discord Gateway session invalidated");
                                return Err(anyhow::anyhow!("Discord session invalidated"));
                            }

                            // Reconnect request
                            7 => {
                                warn!("Discord Gateway requesting reconnect");
                                return Err(anyhow::anyhow!("Discord requested reconnect"));
                            }

                            _ => {
                                debug!(op = payload.op, "Unknown gateway opcode");
                            }
                        }
                    }

                    Some(Ok(WsMessage::Ping(data))) => {
                        let _ = ws_sink.send(WsMessage::Pong(data)).await;
                    }

                    Some(Ok(WsMessage::Close(Some(frame)))) => {
                        let close_code: u16 = frame.code.into();
                        warn!(
                            code = close_code,
                            reason = %frame.reason,
                            "Discord Gateway connection closed by server"
                        );
                        return Ok(());
                    }

                    Some(Ok(WsMessage::Close(None))) => {
                        warn!("Discord Gateway connection closed by server (no close frame)");
                        return Ok(());
                    }

                    Some(Ok(WsMessage::Pong(_))) => {
                        debug!("Pong received");
                    }

                    Some(Err(e)) => {
                        warn!("WebSocket error: {}", e);
                        return Err(anyhow::anyhow!("WebSocket error: {}", e));
                    }

                    None => {
                        warn!("WebSocket stream ended");
                        return Err(anyhow::anyhow!("WebSocket stream ended"));
                    }

                    _ => {}
                }
            }

            // Heartbeat timer — fires reliably even if events are streaming in
            _ = tokio::time::sleep(sleep_duration) => {
                if matches!(state, ConnectionState::Connected | ConnectionState::WaitingForReady) {
                    let heartbeat = json!({
                        "op": 1,
                        "d": sequence
                    });
                    if let Err(e) = ws_sink.send(WsMessage::Text(heartbeat.to_string())).await {
                        warn!("Failed to send heartbeat: {}", e);
                        return Err(anyhow::anyhow!("Failed to send heartbeat: {}", e));
                    }
                    debug!("Heartbeat sent");
                    next_heartbeat = tokio::time::Instant::now()
                        + std::time::Duration::from_millis(heartbeat_interval);
                }
            }
        }
    }
}

/// Strip prefix from message content (case-insensitive), only if it appears at the start.
fn strip_prefix(content: &str, prefix: &str) -> String {
    let lower = content.to_lowercase();
    let lower_prefix = prefix.to_lowercase();
    if lower.starts_with(&lower_prefix) {
        let prefix_chars = prefix.chars().count();
        let rest: String = content.chars().skip(prefix_chars).collect();
        rest.trim().to_string()
    } else {
        content.to_string()
    }
}

/// Strip Discord mention from message content.
/// Mentions appear as `<@USER_ID>` or `<@!USER_ID>` (with nickname)
fn strip_mention(content: &str, bot_id: &str) -> String {
    let mention_patterns = vec![
        format!("<@{}>", bot_id),      // Normal mention
        format!("<@!{}>", bot_id),     // Nickname mention
    ];

    let mut result = content.to_string();
    for pattern in mention_patterns {
        result = result.replace(&pattern, "");
    }

    // Trim leading/trailing whitespace
    result.trim().to_string()
}
