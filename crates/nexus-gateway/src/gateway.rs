use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::{mpsc, watch, Mutex};
use anyhow::Result;
use tracing::{info, warn};

use pocketflow_core::SharedStore;

use crate::messages::{InboundMessage, OutboundMessage};
use crate::plugin::ChannelPlugin;

/// Shared key used to store pending inbound messages in SharedStore.
/// Kept compatible with existing `nexus-chat` usage.
pub const KEY_HUMAN_MESSAGES: &str = "human_messages";



pub struct Gateway {
    plugins: HashMap<String, Arc<dyn ChannelPlugin>>,
    inbound_tx: mpsc::Sender<InboundMessage>,
    inbound_rx: Mutex<Option<mpsc::Receiver<InboundMessage>>>,
    store: SharedStore,
    shutdown_tx: watch::Sender<bool>,
    shutdown_rx: watch::Receiver<bool>,
}

impl Gateway {
    pub fn new(store: SharedStore) -> Self {
        let (inbound_tx, inbound_rx) = mpsc::channel::<InboundMessage>(100);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);
        Self {
            plugins: HashMap::new(),
            inbound_tx,
            inbound_rx: Mutex::new(Some(inbound_rx)),
            store,
            shutdown_tx,
            shutdown_rx,
        }
    }

    pub fn register_plugin(&mut self, plugin: Arc<dyn ChannelPlugin>) {
        let id = plugin.channel_id().to_string();
        info!(channel_id = %id, "Registered channel plugin");
        self.plugins.insert(id, plugin);
    }

    /// Start listeners for all registered plugins.
    pub async fn start_listeners(&self) -> Vec<tokio::task::JoinHandle<()>> {
        let mut handles = Vec::new();
        for (id, plugin) in &self.plugins {
            let tx = self.inbound_tx.clone();
            let rx = self.shutdown_rx.clone();
            let plugin = Arc::clone(plugin);
            let id = id.clone();

            let handle = tokio::spawn(async move {
                if let Err(e) = plugin.start_listener(tx, rx).await {
                    warn!(channel = %id, error = %e, "Listener task ended with error");
                }
            });
            handles.push(handle);
        }
        handles
    }

    /// Await the next inbound message (blocking).
    pub async fn recv_inbound(&self) -> Option<InboundMessage> {
        let mut guard = self.inbound_rx.lock().await;
        if let Some(ref mut rx) = guard.as_mut() {
            rx.recv().await
        } else {
            None
        }
    }

    /// Non-blocking check for inbound messages.
    pub fn try_recv_inbound(&self) -> Option<InboundMessage> {
        let mut guard = self.inbound_rx.try_lock().ok()?;
        if let Some(ref mut rx) = guard.as_mut() {
            rx.try_recv().ok()
        } else {
            None
        }
    }

    /// Route an outbound message to the target plugin(s).
    pub async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        let target = msg.target_channel.as_deref();
        let mut last_error = None;

        if let Some(tid) = target {
            if let Some(plugin) = self.plugins.get(tid) {
                if let Err(e) = plugin.send(msg).await {
                    last_error = Some(e);
                }
            } else {
                warn!(target = %tid, "No plugin registered for target channel");
            }
        } else {
            // Broadcast to all plugins
            for (id, plugin) in &self.plugins {
                if let Err(e) = plugin.send(msg).await {
                    warn!(channel = %id, error = %e, "Failed to send outbound message");
                    last_error = Some(e);
                }
            }
        }

        // Emit event to store
        self.store
            .emit("nexus_gateway", "message_sent", serde_json::to_value(msg).unwrap_or_default())
            .await;

        if let Some(e) = last_error {
            Err(e)
        } else {
            Ok(())
        }
    }

    /// Broadcast to all plugins.
    pub async fn broadcast(&self, msg: &OutboundMessage) -> Result<()> {
        let mut last_error = None;
        for (id, plugin) in &self.plugins {
            if let Err(e) = plugin.send(msg).await {
                warn!(channel = %id, error = %e, "Failed to broadcast message");
                last_error = Some(e);
            }
        }
        if let Some(e) = last_error {
            Err(e)
        } else {
            Ok(())
        }
    }

    pub fn active_channels(&self) -> Vec<String> {
        self.plugins.keys().cloned().collect()
    }

    pub async fn shutdown(&self) -> Result<()> {
        let _ = self.shutdown_tx.send(true);
        info!("Gateway shutdown signal sent");
        Ok(())
    }

    pub fn store(&self) -> &SharedStore {
        &self.store
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::{OutboundMessage, OutboundMessageType};
    use crate::plugin::{ChannelPlugin, GatewayConfig};
    use async_trait::async_trait;

    struct DummyPlugin;

    #[async_trait]
    impl ChannelPlugin for DummyPlugin {
        fn channel_id(&self) -> &str { "dummy" }
        async fn start_listener(&self, _tx: mpsc::Sender<InboundMessage>, _shutdown: watch::Receiver<bool>) -> Result<()> { Ok(()) }
        async fn send(&self, _msg: &OutboundMessage) -> Result<()> { Ok(()) }
        async fn ask_human(&self, _q: &str, _opts: &[&str], _ticket: &str, _timeout: u64) -> Option<String> { None }
    }

    #[tokio::test]
    async fn test_gateway_lifecycle() {
        let store = SharedStore::new_in_memory();
        let mut gateway = Gateway::new(store.clone());
        gateway.register_plugin(Arc::new(DummyPlugin));

        assert_eq!(gateway.active_channels(), vec!["dummy"]);

        let _handles = gateway.start_listeners().await;
        gateway.shutdown().await.unwrap();
    }

    #[tokio::test]
    async fn test_gateway_broadcast() {
        let store = SharedStore::new_in_memory();
        let mut gateway = Gateway::new(store);
        gateway.register_plugin(Arc::new(DummyPlugin));

        let msg = OutboundMessage {
            message_type: OutboundMessageType::StatusUpdate,
            target_channel: None,
            target_conversation: None,
            content: "test".to_string(),
            ticket_id: None,
            worker_id: None,
            metadata: serde_json::Value::Null,
        };

        gateway.broadcast(&msg).await.unwrap();
    }
}
