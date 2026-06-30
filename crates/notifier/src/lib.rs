// crates/notifier/src/lib.rs
//! Human notification service for OpenFlows `awaiting_human` escalations.
//!
//! Supports Slack webhooks, Discord webhooks, and WhatsApp (Twilio).
//! Fires-and-forgets: errors are logged but do not fail the main orchestration loop.
//! Batching is enforced (max 1 per channel per 5 minutes per ticket).

use anyhow::{Context, Result};
use reqwest::Client;
use serde::Serialize;
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, info, warn};

const DEFAULT_COOLDOWN_SECS: u64 = 300; // 5 minutes

/// An escalation notification message.
#[derive(Debug, Clone, Serialize)]
pub struct NotificationMessage {
    /// Ticket identifier, e.g. "T-42".
    pub ticket_id: String,
    /// Agent role that triggered the escalation.
    pub role: String,
    /// Human-readable reason for the escalation.
    pub reason: String,
    /// Link to the Coder workspace or chat.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub workspace_link: String,
    /// Link to the GitHub issue or PR.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub github_link: String,
}

/// A cooldown tracker that prevents duplicate notifications per channel per ticket.
struct Cooldown {
    last_sent: std::collections::HashMap<String, std::time::Instant>,
    cooldown_secs: u64,
}

impl Cooldown {
    fn new(cooldown_secs: u64) -> Self {
        Self {
            last_sent: HashMap::new(),
            cooldown_secs,
        }
    }

    fn key(channel: &str, msg: &NotificationMessage) -> String {
        format!("{}:{}", channel, msg.ticket_id)
    }

    fn is_ready(&self, channel: &str, msg: &NotificationMessage) -> bool {
        let k = Self::key(channel, msg);
        if let Some(last) = self.last_sent.get(&k) {
            last.elapsed().as_secs() >= self.cooldown_secs
        } else {
            true
        }
    }

    fn mark_sent(&mut self, channel: &str, msg: &NotificationMessage) {
        let k = Self::key(channel, msg);
        self.last_sent.insert(k, std::time::Instant::now());
    }
}

/// Notification channel webhook configuration.
#[derive(Debug, Clone)]
pub enum NotificationChannel {
    SlackWebhook { url: String },
    DiscordWebhook { url: String },
    WhatsApp { account_sid: String, auth_token: String, from_phone: String, to_phone: String },
}

/// The notification service that dispatches messages to configured channels.
pub struct NotificationService {
    channels: Vec<NotificationChannel>,
    http: Client,
    cooldown: Arc<RwLock<Cooldown>>,
}

impl NotificationService {
    /// Create a new service from a list of channel configurations.
    pub fn new(channels: Vec<NotificationChannel>) -> Self {
        Self {
            channels,
            http: Client::new(),
            cooldown: Arc::new(RwLock::new(Cooldown::new(DEFAULT_COOLDOWN_SECS))),
        }
    }

    /// Create with a custom cooldown duration in seconds.
    pub fn with_cooldown_secs(self, secs: u64) -> Self {
        Self {
            cooldown: Arc::new(RwLock::new(Cooldown::new(secs))),
            ..self
        }
    }

    /// Send a notification to all configured channels (fire-and-forget).
    /// Errors are logged but do not propagate.
    pub async fn notify(&self, msg: &NotificationMessage) {
        for channel in &self.channels {
            let channel_name = match channel {
                NotificationChannel::SlackWebhook { .. } => "slack",
                NotificationChannel::DiscordWebhook { .. } => "discord",
                NotificationChannel::WhatsApp { .. } => "whatsapp",
            };

            let mut lock = self.cooldown.write().await;
            if !lock.is_ready(channel_name, msg) {
                debug!(
                    channel = channel_name,
                    ticket_id = %msg.ticket_id,
                    "Notification suppressed by cooldown"
                );
                continue;
            }
            lock.mark_sent(channel_name, msg);
            drop(lock);

            let http = self.http.clone();
            let msg = msg.clone();
            let channel = channel.clone();

            tokio::spawn(async move {
                if let Err(e) = send_to_channel(&http, &channel, &msg).await {
                    warn!(
                        channel = channel_name,
                        ticket_id = %msg.ticket_id,
                        error = %e,
                        "Failed to send escalation notification"
                    );
                }
            });
        }
    }
}

async fn send_to_channel(http: &Client, channel: &NotificationChannel, msg: &NotificationMessage) -> Result<()> {
    match channel {
        NotificationChannel::SlackWebhook { url } => send_slack(http, url, msg).await,
        NotificationChannel::DiscordWebhook { url } => send_discord(http, url, msg).await,
        NotificationChannel::WhatsApp { account_sid, auth_token, from_phone, to_phone } => {
            send_whatsapp(http, account_sid, auth_token, from_phone, to_phone, msg).await
        }
    }
}

/// Send a Slack Block Kit message to a webhook URL.
async fn send_slack(http: &Client, url: &str, msg: &NotificationMessage) -> Result<()> {
    let body = serde_json::json!({
        "blocks": [
            {
                "type": "header",
                "text": {
                    "type": "plain_text",
                    "text": format!("🚨 OpenFlows: {} requires human attention", msg.role.to_uppercase()),
                    "emoji": true
                }
            },
            {
                "type": "section",
                "text": {
                    "type": "mrkdwn",
                    "text": format!(
                        "*Ticket:* {}\n*Reason:* {}\n*Workspace:* {}\n*GitHub:* {}",
                        msg.ticket_id, msg.reason,
                        if msg.workspace_link.is_empty() { "N/A".to_string() } else { msg.workspace_link.clone() },
                        if msg.github_link.is_empty() { "N/A".to_string() } else { msg.github_link.clone() }
                    )
                }
            }
        ]
    });

    let resp = http.post(url).json(&body).send().await
        .context("Slack webhook POST failed")?;
    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Slack webhook returned {}: {}", status, body);
    }

    info!(ticket_id = %msg.ticket_id, "Slack notification sent");
    Ok(())
}

struct EmbedField<'a> {
    name: &'a str,
    value: &'a str,
    inline: bool,
}

/// Send a Discord rich embed to a webhook URL.
async fn send_discord(http: &Client, url: &str, msg: &NotificationMessage) -> Result<()> {
    let mut fields = vec![
        EmbedField { name: "Ticket", value: &msg.ticket_id, inline: true },
        EmbedField { name: "Role", value: &msg.role, inline: true },
    ];
    if !msg.workspace_link.is_empty() {
        fields.push(EmbedField { name: "Workspace", value: &msg.workspace_link, inline: false });
    }
    if !msg.github_link.is_empty() {
        fields.push(EmbedField { name: "GitHub", value: &msg.github_link, inline: false });
    }

    let body = serde_json::json!({
        "embeds": [{
            "title": format!("OpenFlows: {} requires human attention", msg.role.to_uppercase()),
            "description": msg.reason,
            "color": 16711680,
            "fields": fields.iter().map(|f| {
                serde_json::json!({
                    "name": f.name,
                    "value": f.value,
                    "inline": f.inline
                })
            }).collect::<Vec<_>>(),
        }]
    });

    let resp = http.post(url).json(&body).send().await
        .context("Discord webhook POST failed")?;
    if !resp.status().is_success() && resp.status().as_u16() != 204 {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Discord webhook returned {}: {}", status, body);
    }

    info!(ticket_id = %msg.ticket_id, "Discord notification sent");
    Ok(())
}

/// Send a WhatsApp message via the Twilio API.
async fn send_whatsapp(
    http: &Client,
    account_sid: &str,
    auth_token: &str,
    from_phone: &str,
    to_phone: &str,
    msg: &NotificationMessage,
) -> Result<()> {
    let url = format!(
        "https://api.twilio.com/2010-04-01/Accounts/{}/Messages.json",
        account_sid
    );

    let mut body_text = format!(
        "OpenFlows Alert: {} (Ticket {})\nReason: {}",
        msg.role.to_uppercase(), msg.ticket_id, msg.reason
    );
    if !msg.workspace_link.is_empty() {
        body_text.push_str(&format!("\nWorkspace: {}", msg.workspace_link));
    }
    if !msg.github_link.is_empty() {
        body_text.push_str(&format!("\nGitHub: {}", msg.github_link));
    }

    let form = [
        ("From", from_phone),
        ("To", to_phone),
        ("Body", &body_text),
    ];

    let resp = http.post(&url)
        .basic_auth(account_sid, Some(auth_token))
        .form(&form)
        .send()
        .await
        .context("Twilio POST failed")?;

    if !resp.status().is_success() {
        let status = resp.status();
        let body = resp.text().await.unwrap_or_default();
        anyhow::bail!("Twilio returned {}: {}", status, body);
    }

    info!(ticket_id = %msg.ticket_id, "WhatsApp notification sent via Twilio");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_notification_message_serialization() {
        let msg = NotificationMessage {
            ticket_id: "T-42".into(),
            role: "forge".into(),
            reason: "Agent is blocked on API auth".into(),
            workspace_link: "https://coder.dev/ws/forge-42".into(),
            github_link: "https://github.com/org/repo/issues/42".into(),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("T-42"));
    }
}
