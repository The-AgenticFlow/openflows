use anyhow::{bail, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{info, warn, debug};

use crate::messages::{InboundMessage, OutboundMessage, OutboundMessageType};
use crate::plugin::ChannelPlugin;
use tokio::sync::{mpsc, watch};

/// Session state that persists across Gateway reconnection attempts.
/// Supports Discord's Resume flow: if `session_id` is `Some`, we send
/// Resume (opcode 6) instead of Identify (opcode 2), letting Discord
/// replay missed events instead of starting fresh.
struct GatewaySession {
    session_id: Option<String>,
    resume_url: Option<String>,
    bot_user_id: Option<String>,
    bot_username: Option<String>,
    sequence: Arc<AtomicU64>,
}

impl GatewaySession {
    fn new() -> Self {
        Self {
            session_id: None,
            resume_url: None,
            bot_user_id: None,
            bot_username: None,
            sequence: Arc::new(AtomicU64::new(0)),
        }
    }

    /// Reset all session state for a fresh Identify (e.g. after clean close or invalid session).
    fn reset(&mut self) {
        self.session_id = None;
        self.resume_url = None;
        self.bot_user_id = None;
        self.bot_username = None;
        // Note: sequence is intentionally NOT reset — Discord may still
        // accept a Resume even after some failures, and resetting it
        // would break the next Resume attempt.
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DiscordMessage {
    id: String,
    content: String,
    author_id: Option<String>,
    channel_id: String,
    message_reference: Option<String>,
}

pub struct DiscordPlugin {
    client: Client,
    bot_token: String,
    channel_id: String,
}

impl DiscordPlugin {
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
            OutboundMessageType::PrOpened => {
                let pr_number = msg.metadata.get("pr_number").and_then(|v| v.as_u64());
                let branch = msg.metadata.get("branch").and_then(|v| v.as_str()).unwrap_or("?");
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
                let reason = msg.metadata.get("reason").and_then(|v| v.as_str()).unwrap_or("");
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
                let files = msg.metadata.get("conflicted_files")
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
                let reason = msg.metadata.get("reason").and_then(|v| v.as_str()).unwrap_or("Unknown");
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
            _ => vec![],
        }
    }

    async fn send_to_discord(&self, msg: &OutboundMessage) -> Result<()> {
        let text = self.format_message(msg);
        let embeds = self.build_embeds(msg);

        let response = self
            .client
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

        info!(message_type = ?msg.message_type, "Sent Discord message");
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
        shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        run_discord_gateway(self.bot_token.clone(), self.channel_id.clone(), tx, shutdown).await
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

// ── Discord Gateway (WebSocket) ─────────────────────────────────────────────

use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{connect_async, tungstenite::Message as WsMessage};

// Discord Gateway intents: GUILD_MESSAGES = 1 << 9 = 512, MESSAGE_CONTENT = 1 << 15 = 32768
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
    session_id: String,
    user: GatewayUser,
    #[serde(rename = "resume_gateway_url", default)]
    resume_gateway_url: Option<String>,
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
    id: String,
    content: String,
    channel_id: String,
    author: MessageCreateAuthor,
    #[serde(default)]
    mentions: Vec<MentionUser>,
}

async fn run_discord_gateway(
    token: String,
    target_channel: String,
    message_tx: mpsc::Sender<InboundMessage>,
    mut shutdown_rx: watch::Receiver<bool>,
) -> Result<()> {
    info!("Discord Gateway background task starting");
    let mut backoff_secs = 1u64;
    let mut session = GatewaySession::new();

    loop {
        if *shutdown_rx.borrow() {
            info!("Discord Gateway shutdown requested, exiting reconnect loop");
            break Ok(());
        }

        info!("Discord Gateway connecting (attempt)");
        match run_gateway_once(
            token.clone(),
            target_channel.clone(),
            message_tx.clone(),
            shutdown_rx.clone(),
            &mut session,
        )
        .await
        {
            Ok(()) => {
                if *shutdown_rx.borrow() {
                    info!("Discord Gateway shut down gracefully");
                    break Ok(());
                }
                warn!("Discord Gateway connection closed, will reconnect");
                // Clean close — server won't accept Resume; start fresh
                session.reset();
            }
            Err(e) => {
                warn!("Discord Gateway connection error: {}", e);
                if *shutdown_rx.borrow() {
                    break Ok(());
                }
                // On Invalid Session (opcode 9), clear session for fresh Identify
                if e.to_string().contains("session invalidated") {
                    session.reset();
                }
                // On reconnect request (opcode 7), keep session for Resume
            }
        }

        warn!(seconds = backoff_secs, "Discord Gateway reconnecting with backoff");
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

/// Run one connection cycle to the Discord Gateway.
///
/// Architecture: three concurrent tasks
/// 1. **Reader** (this function's main loop): reads from WebSocket, dispatches events
/// 2. **Writer** (spawned): reads from an mpsc channel and writes to ws_sink
/// 3. **Heartbeat** (spawned after Hello): sends heartbeats via writer channel on a
///    dedicated `tokio::time::interval`, so heartbeats are NEVER delayed by slow
///    WebSocket reads (the root cause of the ~45s connection resets)
async fn run_gateway_once(
    token: String,
    target_channel: String,
    message_tx: mpsc::Sender<InboundMessage>,
    mut shutdown_rx: watch::Receiver<bool>,
    session: &mut GatewaySession,
) -> Result<()> {
    info!("run_gateway_once: connecting to Discord Gateway");

    // Use resume URL if available (Discord provides this in the READY event),
    // otherwise use the standard gateway URL.
    let connect_url = session.resume_url.as_deref().unwrap_or(GATEWAY_URL);

    let (ws_stream, _) = connect_async(connect_url).await?;
    let (ws_sink, ws_stream) = ws_stream.split();
    info!(url = connect_url, "run_gateway_once: WebSocket connected");

    // ── Outgoing message channel ────────────────────────────────────────
    // The writer task reads from this channel and forwards to ws_sink.
    // This decouples writes from reads — the heartbeat task and the reader
    // both send through this channel, and the writer is the sole owner of
    // ws_sink.
    let (outgoing_tx, outgoing_rx) = mpsc::channel::<WsMessage>(32);

    // ── Writer task ─────────────────────────────────────────────────────
    // Reads WsMessage from the outgoing channel and writes to ws_sink.
    // Exits when all senders are dropped (main loop exits) or on write error.
    let writer_handle: tokio::task::JoinHandle<()> = tokio::spawn(async move {
        let mut sink = ws_sink;
        let mut rx = outgoing_rx;
        while let Some(msg) = rx.recv().await {
            if sink.send(msg).await.is_err() {
                warn!("WebSocket write error in writer task");
                break;
            }
        }
        debug!("Writer task exiting (outgoing channel closed)");
        let _ = sink.close().await;
    });

    let mut hb_handle: Option<tokio::task::JoinHandle<()>> = None;

    let result = async {
        let mut ws_stream = std::pin::pin!(ws_stream);

        loop {
            tokio::select! {
                _ = shutdown_rx.changed() => {
                    info!("Discord Gateway received shutdown signal");
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

                            // Update shared sequence number (used by heartbeat task)
                            if let Some(seq) = payload.sequence {
                                session.sequence.store(seq, Ordering::Relaxed);
                            }

                            match payload.op {
                                1 => { // Heartbeat Request — send immediately
                                    let seq = session.sequence.load(Ordering::Relaxed);
                                    let heartbeat = json!({
                                        "op": 1,
                                        "d": if seq > 0 { Some(seq) } else { None::<u64> }
                                    });
                                    if outgoing_tx.send(WsMessage::Text(heartbeat.to_string())).await.is_err() {
                                        warn!("Failed to queue heartbeat response");
                                    }
                                    info!("Immediate heartbeat queued (opcode 1 request)");
                                }

                                10 => { // Hello
                                    if let Some(data) = payload.data {
                                        if let Ok(hello) = serde_json::from_value::<HelloData>(data) {
                                            let interval = hello.heartbeat_interval;
                                            info!(interval, "Discord Gateway Hello received");

                                            // Spawn heartbeat task with dedicated interval timer.
                                            // This is the key fix: heartbeats now fire on time
                                            // regardless of how long ws_stream.next() blocks.
                                            //
                                            // IMPORTANT: Clone outgoing_tx INSIDE the spawn
                                            // closure so there is no stray Sender left in
                                            // run_gateway_once's scope.  A leftover Sender
                                            // would keep the writer task alive after cleanup,
                                            // causing writer_handle.await to hang forever
                                            // (the bug that prevented reconnection).
                                            let hb_seq = session.sequence.clone();
                                            let hb_out = outgoing_tx.clone();
                                            let mut hb_shut = shutdown_rx.clone();
                                            hb_handle = Some(tokio::spawn(async move {
                                                // First heartbeat: wait jitter (0–10% of interval, capped at 5s)
                                                // per Discord docs to avoid thundering herd after outages.
                                                let jitter = (interval as f64 * 0.1).min(5000.0) as u64;
                                                tokio::select! {
                                                    _ = hb_shut.changed() => return,
                                                    _ = tokio::time::sleep(std::time::Duration::from_millis(jitter)) => {}
                                                }

                                                // Send first heartbeat
                                                let seq = hb_seq.load(Ordering::Relaxed);
                                                let heartbeat = json!({
                                                    "op": 1,
                                                    "d": if seq > 0 { Some(seq) } else { None::<u64> }
                                                });
                                                if hb_out.send(WsMessage::Text(heartbeat.to_string())).await.is_err() {
                                                    return;
                                                }
                                                info!("Initial heartbeat sent (first after Hello)");

                                                // Regular heartbeat interval.
                                                // tokio::time::interval's first tick completes
                                                // immediately — we must consume it WITHOUT
                                                // sending a heartbeat, otherwise we send two
                                                // heartbeats back-to-back (the jitter one and
                                                // the immediate-tick one), which pushes the
                                                // next heartbeat right to Discord's deadline
                                                // edge and causes connection resets.
                                                let mut timer = tokio::time::interval(
                                                    std::time::Duration::from_millis(interval)
                                                );
                                                timer.set_missed_tick_behavior(
                                                    tokio::time::MissedTickBehavior::Delay
                                                );
                                                timer.tick().await; // consume immediate first tick

                                                loop {
                                                    tokio::select! {
                                                        _ = hb_shut.changed() => return,
                                                        _ = timer.tick() => {
                                                            let seq = hb_seq.load(Ordering::Relaxed);
                                                            let heartbeat = json!({
                                                                "op": 1,
                                                                "d": if seq > 0 { Some(seq) } else { None::<u64> }
                                                            });
                                                            if hb_out.send(WsMessage::Text(heartbeat.to_string())).await.is_err() {
                                                                return;
                                                            }
                                                            info!("Periodic heartbeat sent (seq={})", seq);
                                                        }
                                                    }
                                                }
                                            }));

                                            // Send Identify or Resume
                                            if let Some(ref sid) = session.session_id {
                                                let seq = session.sequence.load(Ordering::Relaxed);
                                                info!(
                                                    session_id = %sid,
                                                    sequence = seq,
                                                    "Attempting Resume with existing session"
                                                );
                                                let resume = json!({
                                                    "op": 6,
                                                    "d": {
                                                        "token": token,
                                                        "session_id": sid,
                                                        "seq": seq
                                                    }
                                                });
                                                outgoing_tx.send(WsMessage::Text(resume.to_string())).await?;
                                            } else {
                                                info!("Sending Identify (no existing session)");
                                                let identify = json!({
                                                    "op": 2,
                                                    "d": {
                                                        "token": token,
                                                        "intents": INTENTS,
                                                        "properties": {
                                                            "os": "linux",
                                                            "browser": "nexus-gateway",
                                                            "device": "nexus-gateway"
                                                        }
                                                    }
                                                });
                                                outgoing_tx.send(WsMessage::Text(identify.to_string())).await?;
                                            }
                                        }
                                    }
                                }

                                11 => { // Heartbeat ACK
                                    info!("Heartbeat ACK received from Discord");
                                }

                                0 => { // Event dispatch
                                    if let Some(event_type) = &payload.event_type {
                                        match event_type.as_str() {
                                            "READY" => {
                                                if let Some(data) = payload.data.clone() {
                                                    if let Ok(ready) = serde_json::from_value::<ReadyData>(data) {
                                                        // Save session info for Resume on reconnect
                                                        session.session_id = Some(ready.session_id.clone());
                                                        session.resume_url = ready.resume_gateway_url.clone();
                                                        session.bot_user_id = Some(ready.user.id.clone());
                                                        session.bot_username = Some(ready.user.username.clone());
                                                        info!(
                                                            bot_id = %ready.user.id,
                                                            bot_name = %ready.user.username,
                                                            session_id = %ready.session_id,
                                                            "Discord Gateway READY - connected as bot"
                                                        );
                                                    }
                                                }
                                            }

                                            "RESUMED" => {
                                                info!("Discord Gateway RESUMED - replayed events complete, session restored");
                                            }

                                            "MESSAGE_CREATE" => {
                                                if let Some(data) = payload.data.clone() {
                                                    if let Ok(msg_data) = serde_json::from_value::<MessageCreateData>(data) {
                                                        info!(
                                                            channel_id = %msg_data.channel_id,
                                                            target_channel = %target_channel,
                                                            author = %msg_data.author.username,
                                                            is_bot = msg_data.author.bot,
                                                            content = %msg_data.content,
                                                            "Discord MESSAGE_CREATE received"
                                                        );

                                                        if msg_data.channel_id != target_channel {
                                                            debug!(
                                                                msg_channel = %msg_data.channel_id,
                                                                target = %target_channel,
                                                                "Discord message from different channel — ignoring"
                                                            );
                                                            continue;
                                                        }

                                                        if msg_data.author.bot {
                                                            debug!("Discord message from bot — ignoring");
                                                            continue;
                                                        }

                                                        let is_mentioned = session.bot_user_id.as_ref().map(|bot_id| {
                                                            msg_data.mentions.iter().any(|m| &m.id == bot_id)
                                                        }).unwrap_or(false);

                                                        let starts_with_bot = session.bot_username.as_ref().map(|name| {
                                                            let lower_content = msg_data.content.to_lowercase();
                                                            let lower_name = name.to_lowercase();
                                                            lower_content.starts_with(&lower_name)
                                                                && (lower_content.len() == lower_name.len()
                                                                    || lower_content[lower_name.len()..].starts_with(char::is_whitespace))
                                                        }).unwrap_or(false);

                                                        if !is_mentioned && !starts_with_bot {
                                                            info!(
                                                                is_mentioned,
                                                                starts_with_bot,
                                                                bot_user_id = ?session.bot_user_id,
                                                                bot_username = ?session.bot_username,
                                                                "Discord message not directed at bot — ignoring (mention the bot or start message with bot name)"
                                                            );
                                                            continue;
                                                        }

                                                        let content = if starts_with_bot {
                                                            strip_prefix(&msg_data.content, session.bot_username.as_ref().unwrap())
                                                        } else if is_mentioned {
                                                            strip_mention(&msg_data.content, session.bot_user_id.as_ref().unwrap())
                                                        } else {
                                                            msg_data.content.clone()
                                                        };

                                                        info!(
                                                            user = %msg_data.author.username,
                                                            content = %content,
                                                            "Discord message accepted — routing to gateway"
                                                        );

                                                        let human_msg = InboundMessage {
                                                            message_id: msg_data.id,
                                                            channel_id: "discord".to_string(),
                                                            user_id: msg_data.author.id,
                                                            conversation_id: msg_data.channel_id,
                                                            text: content,
                                                            timestamp: chrono::Utc::now(),
                                                            metadata: serde_json::Value::Null,
                                                        };
                                                        if let Err(e) = message_tx.send(human_msg).await {
                                                            warn!("Failed to send message to channel: {}", e);
                                                        }
                                                    } else {
                                                        debug!("Failed to parse MESSAGE_CREATE data");
                                                    }
                                                }
                                            }

                                            _ => { debug!(event = %event_type, "Unhandled gateway event"); }
                                        }
                                    }
                                }

                                9 => { // Invalid session
                                    let can_resume = payload.data
                                        .and_then(|d| d.as_bool())
                                        .unwrap_or(false);
                                    if can_resume && session.session_id.is_some() {
                                        warn!("Discord Gateway session invalidated (can_resume=true, will attempt Resume)");
                                    } else {
                                        warn!("Discord Gateway session invalidated (can_resume=false, will fresh Identify)");
                                        session.session_id = None;
                                    }
                                    return Err(anyhow::anyhow!("Discord session invalidated"));
                                }

                                7 => { // Reconnect request
                                    warn!("Discord Gateway requesting reconnect — will attempt Resume");
                                    // Keep session_id for Resume on next connection
                                    return Err(anyhow::anyhow!("Discord requested reconnect"));
                                }

                                _ => { debug!(op = payload.op, "Unknown gateway opcode"); }
                            }
                        }

                        Some(Ok(WsMessage::Ping(data))) => {
                            // Forward Pong through the writer task's channel
                            if outgoing_tx.send(WsMessage::Pong(data)).await.is_err() {
                                warn!("Failed to queue Pong response");
                            }
                        }

                        Some(Ok(WsMessage::Close(Some(frame)))) => {
                            warn!(code = %frame.code, reason = %frame.reason, "Discord Gateway connection closed by server");
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
            }
        }
    }.await;

    // ── Cleanup ─────────────────────────────────────────────────────────
    // Abort the heartbeat task first so it stops sending through the
    // outgoing channel (its hb_out Sender is dropped on cancel).
    if let Some(h) = hb_handle {
        h.abort();
    }
    // Drop ALL remaining Senders for the outgoing channel so the writer
    // task sees recv() return None and exits.  outgoing_tx is the only
    // Sender left in this scope (the heartbeat's hb_out was moved into
    // the spawned task and is dropped by abort).
    drop(outgoing_tx);
    // Abort the writer instead of awaiting it.  The WebSocket is already
    // broken (that's why we're here), so graceful close is pointless,
    // and await could still hang if the abort + Sender drops race.
    writer_handle.abort();
    info!("Discord Gateway run_gateway_once cleanup complete");

    result
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
    let patterns = vec![
        format!("<@{}>", bot_id),
        format!("<@!{}>", bot_id),
    ];
    let mut result = content.to_string();
    for pattern in patterns {
        result = result.replace(&pattern, "");
    }
    result.trim().to_string()
}
