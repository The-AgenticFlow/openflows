use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::info;

use crate::{ChatClient, HumanCommand, NexusMessage};
use anyhow::Result;

#[derive(Debug, Clone)]
pub struct MockChatClient {
    messages: Arc<RwLock<Vec<String>>>,
    pending_commands: Arc<RwLock<Vec<HumanCommand>>>,
}

impl MockChatClient {
    pub fn new() -> Self {
        Self {
            messages: Arc::new(RwLock::new(Vec::new())),
            pending_commands: Arc::new(RwLock::new(Vec::new())),
        }
    }

    pub async fn inject_command(&self, cmd: HumanCommand) {
        let mut pending = self.pending_commands.write().await;
        pending.push(cmd);
    }

    pub async fn get_sent_messages(&self) -> Vec<String> {
        self.messages.read().await.clone()
    }

    pub async fn get_pending_commands(&self) -> Vec<HumanCommand> {
        self.pending_commands.read().await.clone()
    }
}

impl Default for MockChatClient {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait::async_trait]
impl ChatClient for MockChatClient {
    async fn send_message(&self, msg: &NexusMessage) -> Result<()> {
        let mut messages = self.messages.write().await;
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
        let pending = self.pending_commands.read().await;
        for cmd in pending.iter() {
            if cmd.command == crate::MessageType::AnswerQuestion
                && cmd.ticket_id.as_deref() == Some(ticket_id)
            {
                return cmd.payload.clone();
            }
        }
        info!(
            "[MOCK CHAT] Question for ticket {}: {} -> Returning mock answer",
            ticket_id, question
        );
        Some("mock_answer".to_string())
    }

    async fn fetch_commands(&self, _channel_id: &str) -> Result<Vec<HumanCommand>> {
        let mut pending = self.pending_commands.write().await;
        let commands: Vec<HumanCommand> = pending.drain(..).collect();
        Ok(commands)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::MessageType;

    #[tokio::test]
    async fn test_mock_chat_sends_message() {
        let mock = MockChatClient::new();
        let msg = NexusMessage::workflow_started("T-001", "forge-1", "Test message", None);

        mock.send_message(&msg).await.unwrap();

        let messages = mock.get_sent_messages().await;
        assert_eq!(messages.len(), 1);
        assert!(messages[0].contains("WorkflowStarted"));
    }

    #[tokio::test]
    async fn test_mock_chat_inject_command() {
        let mock = MockChatClient::new();
        let cmd = HumanCommand::pause_workflow("T-001", "U123", "C123");
        mock.inject_command(cmd.clone()).await;

        let pending = mock.get_pending_commands().await;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].command, MessageType::PauseWorkflow);
    }
}
