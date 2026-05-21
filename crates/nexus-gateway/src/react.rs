use anyhow::Result;
use tracing::{info, warn};

use agent_client::{AgentDecision, AgentPersona, AgentRunner};
use pocketflow_core::SharedStore;
use serde_json::json;

use crate::messages::{InboundMessage, OutboundMessage, OutboundMessageType, SystemCommand};
use crate::gateway::Gateway;
use crate::knowledge::KnowledgeStore;

/// A single step in the ReAct loop.
#[derive(Debug, Clone)]
pub enum ReActStep {
    Observe { input: String },
    Reason { thought: String },
    Act { action: SystemCommand },
    Respond { message: OutboundMessage },
    Done,
}

/// ReAct (Reasoning + Acting) loop for processing human messages.
///
/// Iteratively observes system state, reasons about it with an LLM,
/// acts on the system, and optionally responds back through the Gateway.
pub struct ReActLoop {
    runner: AgentRunner,
    persona: AgentPersona,
    knowledge_store: Option<Box<dyn KnowledgeStore>>,
    max_iterations: usize,
}

impl ReActLoop {
    pub fn new(runner: AgentRunner, max_iterations: usize) -> Self {
        let persona = AgentPersona {
            id: "nexus-react".to_string(),
            role: "react".to_string(),
            system_prompt: REACT_SYSTEM_PROMPT.to_string(),
        };
        Self {
            runner,
            persona,
            knowledge_store: None,
            max_iterations,
        }
    }

    pub fn with_knowledge_store(mut self, store: Box<dyn KnowledgeStore>) -> Self {
        self.knowledge_store = Some(store);
        self
    }

    /// Consume the ReActLoop and return the underlying AgentRunner.
    ///
    /// This allows the caller to reuse the same AgentRunner across multiple
    /// ReActLoop instances, avoiding repeated MCP server spawning.
    pub fn into_runner(self) -> AgentRunner {
        self.runner
    }

    /// Run the ReAct loop for a single inbound message.
    pub async fn run(&mut self, msg: &InboundMessage, store: &SharedStore, gateway: Option<&Gateway>) -> Result<Vec<ReActStep>> {
        let mut steps = Vec::new();

        // Step 1: Observe
        let observation = self.observe(store, msg).await?;
        steps.push(ReActStep::Observe { input: observation.clone() });

        let mut iteration = 0;
        loop {
            if iteration >= self.max_iterations {
                warn!("ReAct loop reached max iterations ({}) — stopping", self.max_iterations);
                break;
            }
            iteration += 1;

            // Step 2: Reason
            let thought = self.reason(&steps).await?;
            steps.push(ReActStep::Reason { thought: thought.clone() });

            // Step 3: Decide on action or response
            let decision = self.decide(&steps, store).await?;

            match decision {
                ReActDecision::Act { action } => {
                    steps.push(ReActStep::Act { action: action.clone() });
                    if let Err(e) = self.act(&action, store).await {
                        warn!("ReAct act failed: {}", e);
                        // Include the error in the observation for the next iteration
                        let error_obs = format!("Action failed: {}", e);
                        steps.push(ReActStep::Observe { input: error_obs });
                    }
                    // Continue the loop to observe the result
                }
                ReActDecision::Respond { content } => {
                    let outbound = OutboundMessage {
                        message_type: OutboundMessageType::StatusUpdate,
                        target_channel: Some(msg.channel_id.clone()),
                        target_conversation: Some(msg.conversation_id.clone()),
                        content,
                        ticket_id: None,
                        worker_id: None,
                        metadata: serde_json::Value::Null,
                    };
                    steps.push(ReActStep::Respond { message: outbound.clone() });

                    if let Some(gw) = gateway {
                        if let Err(e) = gw.send(&outbound).await {
                            warn!("Failed to send ReAct response: {}", e);
                        }
                    }
                    break;
                }
                ReActDecision::Done => {
                    steps.push(ReActStep::Done);
                    break;
                }
            }
        }

        Ok(steps)
    }

    /// Gather current system state + inbound message into an observation string.
    async fn observe(&self, store: &SharedStore, msg: &InboundMessage) -> Result<String> {
        let tickets: serde_json::Value = store.get("tickets").await.unwrap_or(json!([]));
        let workers: serde_json::Value = store.get("worker_slots").await.unwrap_or(json!({}));
        let prs: serde_json::Value = store.get("pending_prs").await.unwrap_or(json!([]));

        Ok(format!(
            "Human message from {}: '{}'\n\nCurrent system state:\n- Tickets: {}\n- Workers: {}\n- Pending PRs: {}",
            msg.user_id, msg.text, tickets, workers, prs
        ))
    }

    /// Call the LLM to reason about the current observation + history.
    async fn reason(&mut self, steps: &[ReActStep]) -> Result<String> {
        let mut context = String::new();
        for step in steps {
            match step {
                ReActStep::Observe { input } => context.push_str(&format!("\nObserve: {}", input)),
                ReActStep::Reason { thought } => context.push_str(&format!("\nReason: {}", thought)),
                ReActStep::Act { action } => context.push_str(&format!("\nAct: {:?}", action)),
                ReActStep::Respond { message } => context.push_str(&format!("\nRespond: {}", message.content)),
                ReActStep::Done => context.push_str("\nDone"),
            }
        }

        let prompt = format!(
            "Given the following observation and action history, what is your next thought?\n{}\n\nThink step by step. Provide a concise thought.",
            context
        );

        let ctx = json!({ "prompt": prompt, "format": "text" });
        let decision: AgentDecision = self.runner.run(&self.persona, ctx, 3).await?;

        Ok(decision.notes)
    }

    /// Decide whether to Act, Respond, or be Done based on the reasoning.
    async fn decide(&self, steps: &[ReActStep], _store: &SharedStore) -> Result<ReActDecision> {
        if let Some(ReActStep::Reason { thought }) = steps.last() {
            let lower = thought.to_lowercase();

            // If thought mentions "done" or "no further action", finish quietly
            if lower.contains("done")
                || lower.contains("no further action")
                || lower.contains("complete")
                || lower.contains("nothing more")
            {
                return Ok(ReActDecision::Done);
            }

            // Always respond with the actual thought content — the agent's reasoning
            // IS the meaningful message the user should see. The crude keyword
            // heuristics were discarding informative responses like re-routing
            // confirmations and status updates.
            return Ok(ReActDecision::Respond {
                content: thought.clone(),
            });
        }

        Ok(ReActDecision::Done)
    }

    /// Execute a system command against the store.
    async fn act(&self, action: &SystemCommand, store: &SharedStore) -> Result<()> {
        use config::state::{KEY_TICKETS, KEY_WORKER_SLOTS};
        use config::{Ticket, TicketStatus, WorkerSlot, WorkerStatus};
        use std::collections::HashMap;

        match action {
            SystemCommand::PauseWorkflow { ticket_id } => {
                let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                if let Some(ticket) = tickets.iter_mut().find(|t| t.id == *ticket_id) {
                    let worker_id = match &ticket.status {
                        TicketStatus::InProgress { worker_id } => worker_id.clone(),
                        TicketStatus::Assigned { worker_id } => worker_id.clone(),
                        _ => return Ok(()),
                    };
                    ticket.status = TicketStatus::AwaitingHuman {
                        worker_id: worker_id.clone(),
                        reason: "paused_by_human".to_string(),
                        attempts: 0,
                    };
                    let mut slots: HashMap<String, WorkerSlot> =
                        store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                    if let Some(slot) = slots.get_mut(&worker_id) {
                        slot.status = WorkerStatus::Suspended {
                            ticket_id: ticket_id.clone(),
                            reason: "paused_by_human".to_string(),
                            issue_url: ticket.issue_url.clone(),
                        };
                    }
                    store.set(KEY_WORKER_SLOTS, json!(slots)).await;
                    store.set(KEY_TICKETS, json!(tickets)).await;
                    info!(ticket_id, "Paused by ReAct");
                }
            }
            SystemCommand::ResumeWorkflow { ticket_id } => {
                let mut tickets: Vec<Ticket> = store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                if let Some(ticket) = tickets.iter_mut().find(|t| t.id == *ticket_id) {
                    if let TicketStatus::AwaitingHuman { worker_id, .. } = &ticket.status {
                        let worker_id = worker_id.clone();
                        ticket.status = TicketStatus::Open;
                        ticket.attempts = 0;
                        let mut slots: HashMap<String, WorkerSlot> =
                            store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                        if let Some(slot) = slots.get_mut(&worker_id) {
                            slot.status = WorkerStatus::Idle;
                        }
                        store.set(KEY_WORKER_SLOTS, json!(slots)).await;
                        store.set(KEY_TICKETS, json!(tickets)).await;
                        info!(ticket_id, "Resumed by ReAct");
                    }
                }
            }
            SystemCommand::ApproveCommand { worker_id } => {
                pocketflow_core::command_gate::CommandGate::approve(store, worker_id).await?;
                info!(worker_id, "Approved by ReAct");
            }
            SystemCommand::BlockAgent { worker_id, reason } => {
                let mut slots: HashMap<String, WorkerSlot> =
                    store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                if let Some(slot) = slots.get_mut(worker_id) {
                    if let WorkerStatus::Assigned { ticket_id, .. }
                        | WorkerStatus::Working { ticket_id, .. } = &slot.status
                    {
                        let tid = ticket_id.clone();
                        let mut tickets: Vec<Ticket> =
                            store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                        if let Some(ticket) = tickets.iter_mut().find(|t| t.id == tid) {
                            ticket.status = TicketStatus::AwaitingHuman {
                                worker_id: worker_id.clone(),
                                reason: format!("blocked_by_human: {}", reason),
                                attempts: 0,
                            };
                            store.set(KEY_TICKETS, json!(tickets)).await;
                        }
                        slot.status = WorkerStatus::Suspended {
                            ticket_id: tid.clone(),
                            reason: format!("blocked_by_human: {}", reason),
                            issue_url: None,
                        };
                        store.set(KEY_WORKER_SLOTS, json!(slots)).await;
                        info!(worker_id, "Blocked by ReAct");
                    }
                }
            }
            SystemCommand::RerouteAgent { from_worker, to_worker } => {
                let mut slots: HashMap<String, WorkerSlot> =
                    store.get_typed(KEY_WORKER_SLOTS).await.unwrap_or_default();
                let ticket_id = if let Some(from_slot) = slots.get(from_worker) {
                    match &from_slot.status {
                        WorkerStatus::Assigned { ticket_id, .. }
                        | WorkerStatus::Working { ticket_id, .. } => Some(ticket_id.clone()),
                        _ => None,
                    }
                } else {
                    None
                };
                if let Some(ticket_id) = ticket_id {
                    if let Some(from_slot) = slots.get_mut(from_worker) {
                        from_slot.status = WorkerStatus::Idle;
                    }
                    if let Some(to_slot) = slots.get_mut(to_worker) {
                        let mut tickets: Vec<Ticket> =
                            store.get_typed(KEY_TICKETS).await.unwrap_or_default();
                        if let Some(ticket) = tickets.iter_mut().find(|t| t.id == ticket_id) {
                            ticket.status = TicketStatus::Assigned {
                                worker_id: to_worker.clone(),
                            };
                            to_slot.status = WorkerStatus::Assigned {
                                ticket_id: ticket_id.clone(),
                                issue_url: ticket.issue_url.clone(),
                            };
                            store.set(KEY_TICKETS, json!(tickets)).await;
                        }
                        store.set(KEY_WORKER_SLOTS, json!(slots)).await;
                        info!(from = from_worker, to = to_worker, "Rerouted by ReAct");
                    }
                }
            }
            SystemCommand::AnswerQuestion { ticket_id, answer } => {
                let response_key = format!("human_response:{}", ticket_id);
                store.set(&response_key, json!(answer)).await;
                info!(ticket_id, "Answer recorded by ReAct");
            }
            SystemCommand::StatusQuery => {
                // StatusQuery is a no-op at the act level; the response is handled in decide
            }
            SystemCommand::GeneralMessage { .. } => {
                // No-op
            }
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
enum ReActDecision {
    Act { action: SystemCommand },
    Respond { content: String },
    Done,
}

const REACT_SYSTEM_PROMPT: &str = r#"You are NEXUS, an autonomous agent orchestrator.
You have just received a message from a human operator.

Your job is to reason about what the human wants and what action to take.
You may:
- Pause/resume a workflow ticket
- Approve or block a worker's command
- Reroute work from one worker to another
- Answer a question for a worker
- Provide a status update

Think step by step about the current state and the human's message, then decide what to do.
Be concise. Do not hallucinate actions.
"#;
