use anyhow::Result;
use async_trait::async_trait;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::messages::{InboundMessage, OutboundMessage};
use crate::plugin::ChannelPlugin;
use tokio::sync::{mpsc, watch};

/// Mock plugin for dev/testing. Mirrors the pattern of MockChatClient.
#[derive(Debug, Clone)]
pub struct MockPlugin {
    sent_messages: Arc<RwLock<Vec<String>>>,
    pending_inbound: Arc<RwLock<Vec<InboundMessage>>>,
}

impl MockPlugin {
    pub fn new() -> Self {
        Self {
            sent_messages: Arc::new(RwLock::new(Vec::new())),
            pending_inbound: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn inject_inbound(&self, msg: InboundMessage) {
        let mut pending = self.pending_inbound.write().await;
        pending.push(msg);
    }

    pub async fn get_sent_messages(&self) -> Vec<String> {
        self.sent_messages.read().await.clone()
    }

    pub async fn get_pending_inbound(&self) -> Vec<InboundMessage> {
        self.pending_inbound.read().await.clone()
    }
}

impl Default for MockPlugin {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl ChannelPlugin for MockPlugin {
    fn channel_id(&self) -> &str {
        "mock"
    }

    async fn start_listener(
        &self,
        tx: mpsc::Sender<InboundMessage>,
        mut shutdown: watch::Receiver<bool>,
    ) -> Result<()> {
        loop {
            tokio::select! {
                _ = shutdown.changed() => {
                    if *shutdown.borrow() {
                        break Ok(());
                    }
                }
                _ = tokio::time::sleep(tokio::time::Duration::from_millis(100)) => {
                    let mut pending = self.pending_inbound.write().await;
                    for msg in pending.drain(..) {
                        let _ = tx.send(msg).await;
                    }
                }
            }
        }
    }

    async fn send(&self, msg: &OutboundMessage) -> Result<()> {
        let mut messages = self.sent_messages.write().await;
        let formatted = format!("[MOCK] {:?}: {}", msg.message_type, msg.content);
        messages.push(formatted.clone());
        info!("{}", formatted);
        Ok(())
    }

    async fn ask_human(
        &self,
        question: &str,
        _options: &[&str],
        ticket_id: &str,
        _timeout_secs: u64,
    ) -> Option<String> {
        info!("[MOCK] Question for ticket {}: {}", ticket_id, question);
        Some("mock_answer".to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::messages::OutboundMessageType;

    #[tokio::test]
    async fn test_mock_plugin_send() {
        let plugin = MockPlugin::new();
        let msg = OutboundMessage {
            message_type: OutboundMessageType::StatusUpdate,
            target_channel: None,
            target_conversation: None,
            content: "hello".to_string(),
            ticket_id: None,
            worker_id: None,
            metadata: serde_json::Value::Null,
        };

        plugin.send(&msg).await.unwrap();
        let sent = plugin.get_sent_messages().await;
        assert_eq!(sent.len(), 1);
        assert!(sent[0].contains("hello"));
    }

    #[tokio::test]
    async fn test_mock_plugin_listener() {
        let plugin = MockPlugin::new();
        let (tx, mut rx) = mpsc::channel(10);
        let (shutdown_tx, shutdown_rx) = watch::channel(false);

        let inbound = InboundMessage {
            message_id: "1".to_string(),
            channel_id: "mock".to_string(),
            user_id: "U1".to_string(),
            conversation_id: "C1".to_string(),
            text: "test".to_string(),
            timestamp: chrono::Utc::now(),
            metadata: serde_json::Value::Null,
        };

        plugin.inject_inbound(inbound.clone()).await;

        let plugin_arc = Arc::new(plugin);
        let handle = tokio::spawn(async move { plugin_arc.start_listener(tx, shutdown_rx).await });

        // Wait a bit for the listener to poll and send
        tokio::time::sleep(tokio::time::Duration::from_millis(300)).await;

        let received = rx.try_recv();
        assert!(received.is_ok());
        assert_eq!(received.unwrap().text, "test");

        shutdown_tx.send(true).unwrap();
        handle.await.unwrap().unwrap();
    }
}
