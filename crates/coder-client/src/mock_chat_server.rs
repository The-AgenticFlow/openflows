// crates/coder-client/src/mock_chat_server.rs
//! Mock WebSocket chat server for testing the ChatStream implementation.
//!
//! Provides a lightweight server that mimics Coder's `/api/experimental/chats/{id}/events`
//! WebSocket endpoint, emitting predetermined events for testing.

use futures_util::{SinkExt, StreamExt};
use serde_json::json;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::net::TcpListener;
use tokio::sync::Mutex;
use tokio_tungstenite::tungstenite::Message;
use tracing::{info, warn};

/// A mock chat server that emits predefined events over WebSocket.
pub struct MockChatServer {
    addr: SocketAddr,
    server_handle: Option<tokio::task::JoinHandle<()>>,
    events: Arc<Mutex<Vec<serde_json::Value>>>,
    shutdown: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
}

impl MockChatServer {
    /// Create a new mock server bound to a random available port.
    pub async fn new() -> anyhow::Result<Self> {
        // Bind to random port on localhost
        let listener = TcpListener::bind("127.0.0.1:0").await?;
        let addr = listener.local_addr()?;

        let events = Arc::new(Mutex::new(Vec::new()));
        let shutdown = Arc::new(Mutex::new(None));

        let server_handle = tokio::spawn(Self::run_server(
            listener,
            Arc::clone(&events),
            Arc::clone(&shutdown),
        ));

        Ok(Self {
            addr,
            server_handle: Some(server_handle),
            events,
            shutdown,
        })
    }

    /// Add events that the server will emit to connected clients.
    pub async fn add_events(&self, events: Vec<serde_json::Value>) {
        let mut guard = self.events.lock().await;
        guard.extend(events);
    }

    /// Set the complete event sequence (replaces any existing events).
    pub async fn set_events(&self, events: Vec<serde_json::Value>) {
        let mut guard = self.events.lock().await;
        *guard = events;
    }

    /// Get the base WebSocket URL (ws://127.0.0.1:{port}).
    pub fn ws_url(&self) -> String {
        format!("ws://{}", self.addr)
    }

    /// Get just the host:port part (e.g., "127.0.0.1:8080").
    pub fn addr_str(&self) -> String {
        self.addr.to_string()
    }

    /// Shutdown the server gracefully.
    pub async fn shutdown(self) {
        if let Some(tx) = self.shutdown.lock().await.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.server_handle {
            let _ = handle.await;
        }
    }

    async fn run_server(
        listener: TcpListener,
        events: Arc<Mutex<Vec<serde_json::Value>>>,
        shutdown: Arc<Mutex<Option<tokio::sync::oneshot::Sender<()>>>>,
    ) {
        let (shutdown_tx, mut shutdown_rx) = tokio::sync::oneshot::channel();
        {
            let mut guard = shutdown.lock().await;
            *guard = Some(shutdown_tx);
        }

        loop {
            tokio::select! {
                // Wait for incoming connections
                Ok((stream, peer_addr)) = listener.accept() => {
                    info!(peer = %peer_addr, "Mock chat server: new WS connection");
                    let events_clone = Arc::clone(&events);
                    tokio::spawn(async move {
                        if let Ok(ws_stream) = tokio_tungstenite::accept_async(stream).await {
                            let (mut _ws_sink, _ws_stream) = ws_stream.split();

                            // Emit events to the client sequentially
                            let events_snapshot = {
                                let guard = events_clone.lock().await;
                                guard.clone()
                            };

                            for event in events_snapshot {
                                let msg = Message::Text(serde_json::to_string(&event).unwrap_or_default().into());
                                if let Err(e) = _ws_sink.send(msg).await {
                                    warn!(peer = %peer_addr, error = %e, "Failed to send event");
                                    break;
                                }
                                // Small delay between events to simulate real streaming
                                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
                            }

                            // Always end with a finished event
                            let finished = json!({
                                "type": "finished",
                                "final_output": "Mock server: chat completed"
                            });
                            let _ = _ws_sink.send(Message::Text(serde_json::to_string(&finished).unwrap().into())).await;

                            info!(peer = %peer_addr, "Mock chat server: finished emitting events");
                        }
                    });
                }
                // Shutdown signal
                _ = &mut shutdown_rx => {
                    info!("Mock chat server shutting down");
                    break;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_mock_server_creation() {
        let server = MockChatServer::new()
            .await
            .expect("Failed to create mock server");
        let url = server.ws_url();
        assert!(url.starts_with("ws://127.0.0.1:"));
        // Server is dropped here, which will terminate the task
    }

    #[tokio::test]
    async fn test_mock_server_add_events() {
        let server = MockChatServer::new().await.unwrap();
        server
            .add_events(vec![
                json!({"type": "status_update", "status": "pending"}),
                json!({"type": "text", "content": "Hello from mock"}),
            ])
            .await;

        // Verify events were stored
        let events = server.events.lock().await;
        assert_eq!(events.len(), 2);
        assert_eq!(events[0]["type"], "status_update");
    }
}

#[cfg(test)]
pub mod test_helpers {
    use super::*;

    /// Create a mock server pre-configured with a typical chat event sequence.
    pub async fn create_typical_mock() -> MockChatServer {
        let server = MockChatServer::new()
            .await
            .expect("Failed to create mock server");

        server
            .set_events(vec![
                json!({"type": "status_update", "status": "pending"}),
                json!({"type": "status_update", "status": "running"}),
                json!({"type": "text", "content": "Hello from mock chat server"}),
                json!({"type": "finished", "final_output": "Chat completed successfully"}),
            ])
            .await;

        server
    }
}
