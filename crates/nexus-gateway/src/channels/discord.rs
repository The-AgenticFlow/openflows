use anyhow::{bail, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::json;
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use crate::messages::{InboundMessage, OutboundMessage, OutboundMessageType};
use crate::plugin::ChannelPlugin;

use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_model::id::Id;

pub struct DiscordPlugin {
    http_client: Client,
    bot_token: String,
    channel_id: String,
}

impl DiscordPlugin {
    pub fn new(bot_token: String, channel_id: String) -> Self {
        Self {
            http_client: Client::new(),
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
            OutboundMessageType::PauseWorkflow => {
                format!("⏸️ Workflow paused: {}", msg.content)
            }
            OutboundMessageType::ResumeWorkflow => {
                format!("▶️ Workflow resumed: {}", msg.content)
            }
            OutboundMessageType::AnswerQuestion => {
                format!("💡 Answer: {}", msg.content)
            }
            OutboundMessageType::ApproveCommand => {
                format!("✅ Approved: {}", msg.content)
            }
            OutboundMessageType::RerouteAgent => {
                format!("🔀 Rerouted: {}", msg.content)
            }
            OutboundMessageType::BlockAgent => {
                format!("🚫 Blocked: {}", msg.content)
            }
            OutboundMessageType::PrOpened => {
                format!(
                    "🔀 PR opened for {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::PrMerged => {
                format!(
                    "🎉 PR merged for {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::CiFailed => {
                format!(
                    "🔴 CI failed for {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::CiTimeout => {
                format!(
                    "⏰ CI timed out for {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::CiMissing => {
                format!(
                    "⚠️ No CI for {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::MergeBlocked => {
                format!(
                    "🚫 Merge blocked for {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::ConflictsDetected => {
                format!(
                    "⚡ Conflicts on {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::TicketFailed => {
                format!(
                    "❌ Ticket failed {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::TicketExhausted => {
                format!(
                    "💀 Ticket exhausted {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::FuelExhausted => {
                format!(
                    "⛽ Fuel exhausted for {}: {}",
                    msg.ticket_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::WorkerSuspended => {
                format!(
                    "⏸️ {} suspended: {}",
                    msg.worker_id.as_deref().unwrap_or("?"),
                    msg.content
                )
            }
            OutboundMessageType::HumanIntervention => {
                format!("🆘 Human intervention needed: {}", msg.content)
            }
        }
    }

    fn build_embeds(&self, msg: &OutboundMessage) -> Vec<serde_json::Value> {
        match msg.message_type {
            OutboundMessageType::ApprovalRequest => {
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
            OutboundMessageType::PrOpened => {
                let pr_number = msg.metadata.get("pr_number").and_then(|v| v.as_u64());
                let branch = msg
                    .metadata
                    .get("branch")
                    .and_then(|v| v.as_str())
                    .unwrap_or("?");
                vec![json!({
                    "title": format!("PR #{} Opened", pr_number.unwrap_or(0)),
                    "description": msg.content,
                    "color": 3066993,
                    "fields": [{
                        "name": "Branch",
                        "value": branch,
                        "inline": true
                    }],
                })]
            }
            OutboundMessageType::PrMerged => {
                let pr_number = msg.metadata.get("pr_number").and_then(|v| v.as_u64());
                vec![json!({
                    "title": format!("PR #{} Merged", pr_number.unwrap_or(0)),
                    "description": msg.content,
                    "color": 5763719,
                })]
            }
            OutboundMessageType::CiFailed => {
                let pr_number = msg.metadata.get("pr_number").and_then(|v| v.as_u64());
                let reason = msg
                    .metadata
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                vec![json!({
                    "title": format!("CI Failed — PR #{}", pr_number.unwrap_or(0)),
                    "description": msg.content,
                    "color": 15548997,
                    "fields": [{
                        "name": "Reason",
                        "value": reason,
                        "inline": false
                    }],
                })]
            }
            OutboundMessageType::CiTimeout => {
                let pr_number = msg.metadata.get("pr_number").and_then(|v| v.as_u64());
                vec![json!({
                    "title": format!("CI Timeout — PR #{}", pr_number.unwrap_or(0)),
                    "description": msg.content,
                    "color": 15105570,
                })]
            }
            OutboundMessageType::ConflictsDetected => {
                let pr_number = msg.metadata.get("pr_number").and_then(|v| v.as_u64());
                let files = msg
                    .metadata
                    .get("conflicted_files")
                    .and_then(|v| v.as_array())
                    .map(|arr| {
                        arr.iter()
                            .filter_map(|f| f.as_str())
                            .map(|f| format!("- {}", f))
                            .collect::<Vec<_>>()
                            .join("\n")
                    })
                    .unwrap_or_default();
                vec![json!({
                    "title": format!("Merge Conflicts — PR #{}", pr_number.unwrap_or(0)),
                    "description": msg.content,
                    "color": 15105570,
                    "fields": [{
                        "name": "Conflicted Files",
                        "value": if files.is_empty() { "See PR for details".to_string() } else { files },
                        "inline": false
                    }],
                })]
            }
            OutboundMessageType::MergeBlocked => {
                let pr_number = msg.metadata.get("pr_number").and_then(|v| v.as_u64());
                let reason = msg
                    .metadata
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("Unknown");
                vec![json!({
                    "title": format!("Merge Blocked — PR #{}", pr_number.unwrap_or(0)),
                    "description": msg.content,
                    "color": 15548997,
                    "fields": [{
                        "name": "Reason",
                        "value": reason,
                        "inline": false
                    }],
                })]
            }
            OutboundMessageType::TicketFailed => {
                vec![json!({
                    "title": format!("Ticket Failed — {}", msg.ticket_id.as_deref().unwrap_or("?")),
                    "description": msg.content,
                    "color": 15548997,
                })]
            }
            OutboundMessageType::TicketExhausted => {
                vec![json!({
                    "title": format!("Ticket Exhausted — {}", msg.ticket_id.as_deref().unwrap_or("?")),
                    "description": msg.content,
                    "color": 10038562,
                    "footer": {
                        "text": "Max attempts exceeded — requires human intervention"
                    }
                })]
            }
            OutboundMessageType::FuelExhausted => {
                vec![json!({
                    "title": format!("Fuel Exhausted — {}", msg.ticket_id.as_deref().unwrap_or("?")),
                    "description": msg.content,
                    "color": 10038562,
                    "footer": {
                        "text": "Agent ran out of time/context — may need manual review"
                    }
                })]
            }
            OutboundMessageType::WorkerSuspended => {
                vec![json!({
                    "title": "Worker Suspended — Approval Required",
                    "description": msg.content,
                    "color": 16753920,
                    "footer": {
                        "text": format!("Reply with `approve {}` to approve or `reject {}` to reject",
                            msg.worker_id.as_deref().unwrap_or("?"),
                            msg.worker_id.as_deref().unwrap_or("?"))
                    }
                })]
            }
            OutboundMessageType::HumanIntervention => {
                let worker_id = msg.worker_id.as_deref().unwrap_or("?");
                vec![json!({
                    "title": "⚠️ Human Intervention Required",
                    "description": msg.content,
                    "color": 15158332,
                    "fields": [{
                        "name": "Worker",
                        "value": worker_id,
                        "inline": true
                    }],
                    "footer": {
                        "text": format!("Reply with `approve {}` to retry or `reject {}` to cancel",
                            worker_id, worker_id)
                    }
                })]
            }
            _ => vec![],
        }
    }

    async fn send_to_discord(&self, msg: &OutboundMessage) -> Result<()> {
        let text = self.format_message(msg);
        let embeds = self.build_embeds(msg);

        let response = self
            .http_client
            .post(format!(
                "https://discord.com/api/v10/channels/{}/messages",
                self.channel_id
            ))
            .header("Authorization", format!("Bot {}", self.bot_token))
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

        debug!(message_type = ?msg.message_type, "Sent Discord message");
        Ok(())
    }
}

#[async_trait]
impl ChannelPlugin for DiscordPlugin {
    fn channel_id(&self) -> &str {
        "discord"
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        mut shutdown_rx: watch::Receiver<bool>,
    ) -> Result<()> {
        run_discord_gateway(
            self.bot_token.clone(),
            self.channel_id.clone(),
            tx,
            &mut shutdown_rx,
        )
        .await
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        self.send_to_discord(msg).await
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
        if self.send_to_discord(&msg).await.is_err() {
            return None;
        }
        None
    }
}

// ── Discord Gateway (via twilight-gateway) ──────────────────────────────────
//
// Uses the twilight-gateway crate which handles all the complexity:
// - Automatic heartbeat management
// - Automatic reconnection with Resume support
// - Session invalidation handling
// - Proper backoff on connection failures
//
// This replaces the previous manual WebSocket implementation (~500 lines)
// with a ~100 line wrapper that's far more reliable.

async fn run_discord_gateway(
    token: String,
    target_channel: String,
    message_tx: mpsc::Sender<InboundMessage>,
    shutdown_rx: &mut watch::Receiver<bool>,
) -> Result<()> {
    info!("Discord Gateway starting (twilight-gateway)");

    let target_channel_id: Id<twilight_model::id::marker::ChannelMarker> = {
        let raw: u64 = target_channel.parse().map_err(|e| {
            anyhow::anyhow!("Invalid Discord channel_id '{}': {}", target_channel, e)
        })?;
        Id::new(raw)
    };

    let intents = Intents::GUILD_MESSAGES | Intents::MESSAGE_CONTENT;
    let mut shard = Shard::new(ShardId::ONE, token, intents);

    let mut bot_user_id: Option<Id<twilight_model::id::marker::UserMarker>> = None;
    let mut bot_username: Option<String> = None;

    let event_flags =
        EventTypeFlags::MESSAGE_CREATE | EventTypeFlags::READY | EventTypeFlags::RESUMED;

    loop {
        tokio::select! {
            _ = shutdown_rx.changed() => {
                info!("Discord Gateway shutdown requested");
                break Ok(());
            }
            item = shard.next_event(event_flags) => {
                let Some(item) = item else {
                    debug!("Discord Gateway event stream ended, will reconnect");
                    continue;
                };

                let Ok(event) = item else {
                    warn!("Discord Gateway event error — twilight will handle reconnection");
                    continue;
                };

                match event {
                    Event::Ready(ready) => {
                        let user = &ready.user;
                        bot_user_id = Some(user.id);
                        bot_username = Some(user.name.clone());
                        info!(
                            bot_id = %user.id,
                            bot_name = %user.name,
                            session_id = %ready.session_id,
                            "Discord Gateway READY"
                        );
                    }
                    Event::Resumed => {
                        debug!("Discord Gateway RESUMED — session restored");
                    }
                    Event::MessageCreate(msg) => {
                        if msg.channel_id != target_channel_id {
                            continue;
                        }
                        if msg.author.bot {
                            continue;
                        }

                        // Message must be directed at the bot (mention or username prefix)
                        let is_mentioned = bot_user_id.map(|bid| {
                            msg.mentions.iter().any(|m| m.id == bid)
                        }).unwrap_or(false);

                        let starts_with_bot = bot_username.as_ref().map(|name| {
                            let lower_content = msg.content.to_lowercase();
                            let lower_name = name.to_lowercase();
                            lower_content.starts_with(&lower_name)
                                && (lower_content.len() == lower_name.len()
                                    || lower_content[lower_name.len()..].starts_with(char::is_whitespace))
                        }).unwrap_or(false);

                        if !is_mentioned && !starts_with_bot {
                            continue;
                        }

                        // Strip mention/bot name prefix from content
                        let content = if starts_with_bot {
                            let name = bot_username.as_ref().unwrap();
                            strip_prefix(&msg.content, name)
                        } else if is_mentioned {
                            let bot_id = bot_user_id.map(|id| id.to_string()).unwrap_or_default();
                            strip_mention(&msg.content, &bot_id)
                        } else {
                            msg.content.clone()
                        };

                        debug!(
                            user = %msg.author.name,
                            content = %content,
                            "Discord message accepted"
                        );

                        let human_msg = InboundMessage {
                            message_id: msg.id.to_string(),
                            channel_id: "discord".to_string(),
                            user_id: msg.author.id.to_string(),
                            conversation_id: msg.channel_id.to_string(),
                            text: content,
                            timestamp: chrono::Utc::now(),
                            metadata: serde_json::Value::Null,
                        };
                        if let Err(e) = message_tx.send(human_msg).await {
                            warn!("Failed to send message to channel: {}", e);
                        }
                    }
                    _ => {}
                }
            }
        }
    }
}

/// Strip prefix from message content (case-insensitive).
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
fn strip_mention(content: &str, bot_id: &str) -> String {
    let patterns = vec![format!("<@{}>", bot_id), format!("<@!{}>", bot_id)];
    let mut result = content.to_string();
    for pattern in patterns {
        result = result.replace(&pattern, "");
    }
    result.trim().to_string()
}
