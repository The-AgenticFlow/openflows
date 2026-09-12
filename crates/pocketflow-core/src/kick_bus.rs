// crates/pocketflow-core/src/kick_bus.rs
//! Hook kick bus — a tenant-namespaced wake-up channel for the Controller.
//!
//! Lifecycle hooks (experimental Coder `agent-lifecycle-hooks`) are "hints, not
//! commit proofs": durable state lives in the SharedStore and the Controller
//! reconciles it on its poll loop. The kick bus lets a hook *wake the Controller
//! early* so it re-runs its reconciliation pass the moment a state-mutating event
//! (e.g. a sentinel verdict, a forge phase change, a stop) lands — without waiting
//! the full poll interval. The pass is idempotent, so an early wake only shortens
//! latency; it never changes the outcome.
//!
//! Backends:
//!   - Redis (production): real `PUBLISH` / `SUBSCRIBE` via `fred`.
//!   - In-memory (dev/tests): process-local `tokio::sync::broadcast`.
//!
//! Both expose the same [`HookKickBus`] factory + [`HookKickPublisher`] /
//! [`HookKickReceiver`] halves.

use anyhow::{Context, Result};
use serde_json::Value;
use tracing::{debug, warn};

/// Suffix of the Redis channel a tenant's kicks are published on. The full
/// channel is `openflows:{tenant}:hooks:kick`.
const KICK_CHANNEL_SUFFIX: &str = "hooks:kick";

/// A wake-up notice published by a lifecycle hook consumer.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct HookKick {
    /// Coder chat the event originated from.
    pub chat_id: String,
    /// Coder dispatch id (dedupe).
    #[serde(default)]
    pub dispatch_id: String,
    /// The lifecycle event that produced the kick.
    #[serde(default = "event_default")]
    pub event: String,
    /// Role as resolved from the chat (forge / sentinel / vessel / lore / nexus).
    #[serde(default)]
    pub role: String,
    /// Ticket id if resolvable.
    #[serde(default)]
    pub ticket_id: String,
    /// Short "why" (e.g. `verdict_written`, `phase_changed`).
    #[serde(default)]
    pub hint: String,
    /// Opaque event-specific payload.
    #[serde(default)]
    pub data: Value,
}

fn event_default() -> String {
    "post_tool_use".to_string()
}

impl HookKick {
    /// Build a kick with sensible defaults (event set by caller).
    pub fn new(chat_id: impl Into<String>, event: impl Into<String>) -> Self {
        Self {
            chat_id: chat_id.into(),
            dispatch_id: String::new(),
            event: event.into(),
            role: String::new(),
            ticket_id: String::new(),
            hint: String::new(),
            data: Value::Null,
        }
    }
}

/// The publish half of the kick bus.
#[derive(Clone)]
pub struct HookKickPublisher {
    backend: KickBackend,
}

/// The subscribe half of the kick bus.
pub struct HookKickReceiver {
    backend: KickReceiveBackend,
}

#[derive(Clone)]
enum KickBackend {
    InMemory(tokio::sync::broadcast::Sender<Value>),
    Redis {
        client: fred::clients::Client,
        channel: String,
    },
}

enum KickReceiveBackend {
    InMemory(tokio::sync::broadcast::Receiver<Value>),
    Redis(fred::clients::SubscriberClient),
}

/// The mix of required fred / tokio types.
use fred::prelude::*;

impl HookKickPublisher {
    /// Publish a wake-up notice. Never fails the caller: a missing subscriber,
    /// a closed channel, or a Redis blip are all "just latency" and are logged.
    pub async fn publish(&self, kick: &HookKick) {
        let json = match serde_json::to_value(kick) {
            Ok(v) => v,
            Err(e) => {
                warn!(error = %e, "kick bus: failed to serialize kick");
                return;
            }
        };
        match &self.backend {
            KickBackend::InMemory(tx) => {
                if tx.send(json).is_err() {
                    debug!("kick bus: no in-memory subscribers");
                }
            }
            KickBackend::Redis { client, channel } => {
                let payload = serde_json::to_string(&json).unwrap_or_else(|_| "{}".into());
                match client.publish::<i64, _, _>(channel, payload).await {
                    Ok(n) => debug!(channel = %channel, subscribers = n, "kick bus: published"),
                    Err(e) => warn!(error = %e, channel = %channel, "kick bus: publish failed"),
                }
            }
        }
    }
}

impl HookKickReceiver {
    /// Block until the next kick arrives. Returns `None` if the underlying
    /// channel closed (callers should fall back to the normal poll interval).
    pub async fn recv(&mut self) -> Option<HookKick> {
        match &mut self.backend {
            KickReceiveBackend::InMemory(rx) => match rx.recv().await {
                Ok(v) => parse_kick(v),
                Err(_) => None,
            },
            KickReceiveBackend::Redis(sub) => match sub.message_rx().recv().await {
                Ok(msg) => {
                    let payload = msg.value.into_string().unwrap_or_default();
                    match serde_json::from_str::<HookKick>(&payload) {
                        Ok(k) => Some(k),
                        Err(e) => {
                            debug!(error = %e, "kick bus: dropped non-HookKick message");
                            None
                        }
                    }
                }
                Err(_) => None,
            },
        }
    }
}

fn parse_kick(v: Value) -> Option<HookKick> {
    match serde_json::from_value::<HookKick>(v) {
        Ok(k) => Some(k),
        Err(e) => {
            debug!(error = %e, "kick bus: dropped non-HookKick message");
            None
        }
    }
}

/// Build the kick bus for the given store backend.
///
/// `redis_url` is `None` for the in-memory store. `tenant` namespaces the
/// channel. Returns `(publisher, receiver)`; either half can be dropped.
pub async fn build_kick_bus(
    redis_url: Option<&str>,
    tenant: &str,
) -> Result<(HookKickPublisher, HookKickReceiver)> {
    let channel = format!("openflows:{tenant}::{KICK_CHANNEL_SUFFIX}");

    match redis_url {
        Some(url) => {
            // Publisher: reuse the plain pooled client used by the store.
            let config = Config::from_url(url)?;
            let client = Builder::from_config(config.clone()).build()?;
            client.init().await?;

            // Subscriber: a dedicated pub/sub client.
            let subscriber = Builder::from_config(config)
                .build_subscriber_client()
                .context("failed to build redis subscriber client")?;
            subscriber.init().await?;
            subscriber.subscribe(channel.as_str()).await?;

            let _ = subscriber.manage_subscriptions();

            Ok((
                HookKickPublisher {
                    backend: KickBackend::Redis {
                        client,
                        channel: channel.clone(),
                    },
                },
                HookKickReceiver {
                    backend: KickReceiveBackend::Redis(subscriber),
                },
            ))
        }
        None => {
            let (tx, _rx) = tokio::sync::broadcast::channel::<Value>(256);
            Ok((
                HookKickPublisher {
                    backend: KickBackend::InMemory(tx.clone()),
                },
                HookKickReceiver {
                    backend: KickReceiveBackend::InMemory(tx.subscribe()),
                },
            ))
        }
    }
}
