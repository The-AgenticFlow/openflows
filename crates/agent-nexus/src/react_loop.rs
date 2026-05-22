use anyhow::Result;
use pocketflow_core::SharedStore;
use tracing::{info, warn};

use nexus_gateway::{Gateway, ReActLoop};

/// Process inbound messages from the Gateway using a ReAct loop.
///
/// This is a thin wrapper around `nexus_gateway::ReActLoop` that creates
/// the loop on-demand with an `AgentRunner` resolved from the registry.
///
/// **Important**: This function checks for pending messages BEFORE creating
/// an AgentRunner. If no messages are queued, it returns immediately without
/// the expensive MCP server spawn + FallbackClient initialization.
pub async fn process_gateway_messages(
    gateway: &Gateway,
    store: &SharedStore,
    registry_path: &std::path::Path,
) -> Result<()> {
    // Early return: skip AgentRunner creation when no messages are pending.
    // This prevents spawning a throwaway GitHub MCP server every 3 seconds
    // from the background gateway processor task.
    let first_msg = match gateway.try_recv_inbound() {
        Some(msg) => {
            info!(
                user = %msg.user_id,
                channel = %msg.channel_id,
                text = %msg.text,
                "Gateway: inbound message received in NexusNode prep"
            );
            msg
        }
        None => return Ok(()),
    };

    // Create an AgentRunner for the ReAct loop (only when we have messages)
    let registry = config::Registry::load(registry_path)?;
    let model_backend = registry.get("nexus").and_then(|e| e.model_backend.clone());
    let github_token = registry.resolve_github_token("nexus")?;

    let runner =
        agent_client::AgentRunner::from_env_with_token(model_backend.as_deref(), &github_token)
            .await?;

    let mut react_loop = ReActLoop::new(runner, 8);

    // Process the first message we already dequeued
    process_single_message(&mut react_loop, &first_msg, store, gateway).await;

    // Process any remaining queued messages with the same runner
    while let Some(msg) = gateway.try_recv_inbound() {
        process_single_message(&mut react_loop, &msg, store, gateway).await;
    }

    Ok(())
}

/// Process inbound messages using an existing AgentRunner (reuse across calls).
///
/// Use this from long-running background tasks to avoid creating a new
/// AgentRunner (and GitHub MCP server subprocess) on every iteration.
/// The runner is created lazily on first message and reused until it fails,
/// at which point it is recreated.
pub async fn process_gateway_messages_with_runner(
    gateway: &Gateway,
    store: &SharedStore,
    registry_path: &std::path::Path,
    runner: &mut Option<agent_client::AgentRunner>,
) -> Result<()> {
    // Early return: skip AgentRunner creation when no messages are pending.
    let first_msg = match gateway.try_recv_inbound() {
        Some(msg) => {
            info!(
                user = %msg.user_id,
                channel = %msg.channel_id,
                text = %msg.text,
                "Gateway: inbound message received in background processor"
            );
            msg
        }
        None => return Ok(()),
    };

    // Lazily create the AgentRunner if needed
    if runner.is_none() {
        let registry = match config::Registry::load(registry_path) {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    user = %first_msg.user_id,
                    text = %first_msg.text,
                    error = %e,
                    "Message dropped: failed to load registry for runner creation"
                );
                return Err(e);
            }
        };
        let model_backend = registry.get("nexus").and_then(|e| e.model_backend.clone());
        let github_token = match registry.resolve_github_token("nexus") {
            Ok(t) => t,
            Err(e) => {
                warn!(
                    user = %first_msg.user_id,
                    text = %first_msg.text,
                    error = %e,
                    "Message dropped: failed to resolve GitHub token for runner creation"
                );
                return Err(e);
            }
        };

        let new_runner = match agent_client::AgentRunner::from_env_with_token(
            model_backend.as_deref(),
            &github_token,
        )
        .await
        {
            Ok(r) => r,
            Err(e) => {
                warn!(
                    user = %first_msg.user_id,
                    text = %first_msg.text,
                    error = %e,
                    "Message dropped: failed to create AgentRunner"
                );
                let error_msg = nexus_gateway::messages::OutboundMessage {
                    message_type: nexus_gateway::messages::OutboundMessageType::WorkflowError,
                    target_channel: Some(first_msg.channel_id.clone()),
                    target_conversation: Some(first_msg.conversation_id.clone()),
                    content: format!("❌ Agent failed to initialize: {}", e),
                    ticket_id: None,
                    worker_id: None,
                    metadata: serde_json::json!({ "error": e.to_string(), "user_id": first_msg.user_id }),
                };
                if let Err(send_err) = gateway.send(&error_msg).await {
                    warn!("Failed to send error notification: {}", send_err);
                }
                return Err(e);
            }
        };

        *runner = Some(new_runner);
    }

    // Take ownership of the runner temporarily for ReActLoop
    let r = runner
        .take()
        .expect("runner was just initialized above if None");
    let mut react_loop = ReActLoop::new(r, 8);

    // Process the first message we already dequeued
    process_single_message(&mut react_loop, &first_msg, store, gateway).await;

    // Process any remaining queued messages
    while let Some(msg) = gateway.try_recv_inbound() {
        process_single_message(&mut react_loop, &msg, store, gateway).await;
    }

    // Put the runner back for reuse on the next iteration
    *runner = Some(react_loop.into_runner());

    Ok(())
}

/// Process a single inbound message through the ReAct loop.
///
/// The ReAct loop handles both reasoning and command execution internally
/// (via `ReActLoop::act()`), so there's no need for a separate command
/// executor fallback. Pattern-only interpretation was a pre-ReAct
/// compatibility layer that's now redundant.
async fn process_single_message(
    react_loop: &mut ReActLoop,
    msg: &nexus_gateway::messages::InboundMessage,
    store: &SharedStore,
    gateway: &Gateway,
) {
    info!(
        user = %msg.user_id,
        channel = %msg.channel_id,
        text = %msg.text,
        "Processing inbound message via ReAct loop"
    );

    match react_loop.run(msg, store, Some(gateway)).await {
        Ok(steps) => {
            info!(steps = steps.len(), "ReAct loop completed");
        }
        Err(e) => {
            warn!("ReAct loop failed: {}", e);
            let error_msg = nexus_gateway::messages::OutboundMessage {
                message_type: nexus_gateway::messages::OutboundMessageType::WorkflowError,
                target_channel: Some(msg.channel_id.clone()),
                target_conversation: Some(msg.conversation_id.clone()),
                content: format!("❌ Agent processing failed for your message: {}", e),
                ticket_id: None,
                worker_id: None,
                metadata: serde_json::json!({ "error": e.to_string(), "user_id": msg.user_id }),
            };
            if let Err(send_err) = gateway.send(&error_msg).await {
                warn!("Failed to send error notification: {}", send_err);
            }
        }
    }
}
