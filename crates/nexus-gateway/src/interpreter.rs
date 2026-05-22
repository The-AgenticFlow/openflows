use agent_client::{AgentDecision, AgentPersona, AgentRunner};

use crate::knowledge::{KnowledgeStore, StubKnowledgeStore};
use crate::messages::{InboundMessage, InterpretedCommand, SystemCommand};

/// Layered command interpreter: pattern match → RAG → LLM.
pub struct CommandInterpreter {
    llm_interpreter: Option<LlmInterpreter>,
    knowledge_store: Box<dyn KnowledgeStore>,
}

pub struct LlmInterpreter {
    runner: AgentRunner,
    persona: AgentPersona,
}

impl CommandInterpreter {
    pub fn new_with_llm(runner: AgentRunner) -> Self {
        let persona = AgentPersona {
            id: "nexus-interpreter".to_string(),
            role: "interpreter".to_string(),
            system_prompt: "You are a command parser. Respond ONLY with valid JSON.".to_string(),
        };
        Self {
            llm_interpreter: Some(LlmInterpreter { runner, persona }),
            knowledge_store: Box::new(StubKnowledgeStore),
        }
    }

    pub fn new_pattern_only() -> Self {
        Self {
            llm_interpreter: None,
            knowledge_store: Box::new(StubKnowledgeStore),
        }
    }

    pub fn with_knowledge_store(mut self, store: Box<dyn KnowledgeStore>) -> Self {
        self.knowledge_store = store;
        self
    }

    /// Layered interpretation strategy.
    pub async fn interpret(&mut self, msg: &InboundMessage) -> Option<InterpretedCommand> {
        // 1. Fast pattern match
        if let Some(cmd) = self.try_pattern_match(msg) {
            return Some(cmd);
        }

        // 2. RAG lookup (stub for now)
        let _knowledge = self.knowledge_store.search(&msg.text, 3).await.ok();

        // 3. LLM fallback
        if let Some(ref mut llm) = self.llm_interpreter {
            if let Some(cmd) = llm.interpret(msg).await {
                return Some(cmd);
            }
        }

        None
    }

    fn try_pattern_match(&self, msg: &InboundMessage) -> Option<InterpretedCommand> {
        let text = msg.text.trim();
        let lower = text.to_lowercase();

        let starts_with_cmd = |cmd: &str| lower == cmd || lower.starts_with(&format!("{} ", cmd));
        let fuzzy_cmd = |cmd: &str| {
            let first = lower.split_whitespace().next().unwrap_or("");
            first == cmd || first.starts_with(cmd)
        };

        // pause
        if starts_with_cmd("pause") || fuzzy_cmd("pause") {
            let ticket_id = extract_ticket_id(text);
            return ticket_id.map(|id| InterpretedCommand {
                command: SystemCommand::PauseWorkflow { ticket_id: id },
                source: msg.clone(),
                confidence: 1.0,
            });
        }

        // resume
        if starts_with_cmd("resume") || fuzzy_cmd("resume") {
            let ticket_id = extract_ticket_id(text);
            return ticket_id.map(|id| InterpretedCommand {
                command: SystemCommand::ResumeWorkflow { ticket_id: id },
                source: msg.clone(),
                confidence: 1.0,
            });
        }

        // approve
        if starts_with_cmd("approve") || fuzzy_cmd("approve") {
            let worker_id = extract_worker_id(text).unwrap_or_else(|| "unknown".to_string());
            return Some(InterpretedCommand {
                command: SystemCommand::ApproveCommand { worker_id },
                source: msg.clone(),
                confidence: 1.0,
            });
        }

        // reject / deny
        if starts_with_cmd("reject")
            || starts_with_cmd("deny")
            || fuzzy_cmd("reject")
            || fuzzy_cmd("deny")
        {
            let worker_id = extract_worker_id(text);
            return Some(InterpretedCommand {
                command: SystemCommand::BlockAgent {
                    worker_id: worker_id.unwrap_or_else(|| "unknown".to_string()),
                    reason: "rejected by human".to_string(),
                },
                source: msg.clone(),
                confidence: 1.0,
            });
        }

        // block
        if starts_with_cmd("block") || fuzzy_cmd("block") {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() >= 2 {
                let worker_id = parts[1].to_string();
                let reason = parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                return Some(InterpretedCommand {
                    command: SystemCommand::BlockAgent { worker_id, reason },
                    source: msg.clone(),
                    confidence: 1.0,
                });
            }
        }

        // reroute / reassign / assign
        if starts_with_cmd("reroute")
            || starts_with_cmd("reassign")
            || starts_with_cmd("assign")
            || fuzzy_cmd("reroute")
            || fuzzy_cmd("reassign")
            || fuzzy_cmd("assign")
        {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() >= 3 {
                let (from_worker, to_worker) = extract_two_workers(&parts);
                if from_worker.contains('-') && to_worker.contains('-') {
                    return Some(InterpretedCommand {
                        command: SystemCommand::RerouteAgent {
                            from_worker,
                            to_worker,
                        },
                        source: msg.clone(),
                        confidence: 1.0,
                    });
                }

                // Single worker assignment case
                if parts.len() >= 2 {
                    let worker_id = normalize_worker_id(parts[1]);
                    if worker_id.starts_with("forge-") || worker_id.starts_with("agent-") {
                        return Some(InterpretedCommand {
                            command: SystemCommand::ApproveCommand { worker_id },
                            source: msg.clone(),
                            confidence: 0.8,
                        });
                    }
                }
            }
        }

        // answer
        if starts_with_cmd("answer") || fuzzy_cmd("answer") {
            let parts: Vec<&str> = text.split_whitespace().collect();
            if parts.len() >= 2 {
                let ticket_id = extract_ticket_id(text)
                    .unwrap_or_else(|| parts[1].trim_end_matches(':').to_string());
                let answer = parts.get(2..).map(|p| p.join(" ")).unwrap_or_default();
                return Some(InterpretedCommand {
                    command: SystemCommand::AnswerQuestion { ticket_id, answer },
                    source: msg.clone(),
                    confidence: 1.0,
                });
            }
        }

        // Simple yes/no/option answers
        if lower == "yes" || lower == "no" || lower.starts_with("option ") {
            return Some(InterpretedCommand {
                command: SystemCommand::AnswerQuestion {
                    ticket_id: "unknown".to_string(),
                    answer: text.to_string(),
                },
                source: msg.clone(),
                confidence: 0.6,
            });
        }

        // status query
        if starts_with_cmd("status") || fuzzy_cmd("status") {
            return Some(InterpretedCommand {
                command: SystemCommand::StatusQuery,
                source: msg.clone(),
                confidence: 1.0,
            });
        }

        None
    }
}

impl LlmInterpreter {
    async fn interpret(&mut self, msg: &InboundMessage) -> Option<InterpretedCommand> {
        let prompt = format!(
            r#"You are NEXUS, interpreting a message from a human operator.
Parse the following message into a structured command.

Human message: "{}"

Available action types and required fields:
- pause_workflow: set ticket_id (string, e.g. "T-001")
- resume_workflow: set ticket_id (string)
- approve_command: set assign_to to worker_id (string, e.g. "forge-1")
- block_agent: set assign_to to worker_id (string), put reason in notes (string)
- reroute_agent: set assign_to to from_worker (string, e.g. "forge-1"), put to_worker in notes (string, e.g. "forge-2")
- answer_question: set ticket_id (string), put answer in notes (string)
- status_query: no fields needed
- general_message: no fields needed (use when message is not a command)

RULES:
1. ALL field values must be simple strings or null. NEVER use nested JSON objects.
2. For reroute_agent, put the source worker in "assign_to" and destination worker in "notes".
3. If a worker is mentioned as "forge 1", normalize it to "forge-1".
4. If the message is vague or missing required fields, use general_message.
5. Put any additional context or parameters in the "notes" field.

Respond with ONLY a JSON object matching this schema. No markdown, no explanations.

Example valid responses:
{{"action": "pause_workflow", "notes": "", "assign_to": null, "ticket_id": "T-001", "issue_url": null}}
{{"action": "reroute_agent", "notes": "forge-2", "assign_to": "forge-1", "ticket_id": null, "issue_url": null}}
{{"action": "block_agent", "notes": "stuck on test failure", "assign_to": "forge-1", "ticket_id": null, "issue_url": null}}
{{"action": "general_message", "notes": "", "assign_to": null, "ticket_id": null, "issue_url": null}}"#,
            msg.text
        );

        let context = serde_json::json!({
            "prompt": prompt,
            "format": "json"
        });

        match self.runner.run(&self.persona, context, 5).await {
            Ok(decision) => self.decision_to_command(decision, msg),
            Err(e) => {
                tracing::warn!("LLM interpretation failed: {}", e);
                None
            }
        }
    }

    fn decision_to_command(
        &self,
        decision: AgentDecision,
        msg: &InboundMessage,
    ) -> Option<InterpretedCommand> {
        let cmd = match decision.action.as_str() {
            "pause_workflow" => decision
                .ticket_id
                .map(|id| SystemCommand::PauseWorkflow { ticket_id: id }),
            "resume_workflow" => decision
                .ticket_id
                .map(|id| SystemCommand::ResumeWorkflow { ticket_id: id }),
            "approve_command" | "approve" => {
                let worker_id = decision.assign_to.unwrap_or_else(|| "unknown".to_string());
                Some(SystemCommand::ApproveCommand { worker_id })
            }
            "block_agent" | "block" | "reject" => {
                let worker_id = decision.assign_to.unwrap_or_else(|| "unknown".to_string());
                let reason = if decision.notes.is_empty() {
                    "blocked_by_human".to_string()
                } else {
                    decision.notes
                };
                Some(SystemCommand::BlockAgent { worker_id, reason })
            }
            "reroute_agent" | "reroute" => {
                let from_worker = decision.assign_to.unwrap_or_else(|| "unknown".to_string());
                let to_worker = if decision.notes.is_empty() {
                    "unknown".to_string()
                } else {
                    decision.notes
                };
                Some(SystemCommand::RerouteAgent {
                    from_worker,
                    to_worker,
                })
            }
            "answer_question" => {
                let ticket_id = decision.ticket_id.unwrap_or_else(|| "unknown".to_string());
                let answer = if decision.notes.is_empty() {
                    "yes".to_string()
                } else {
                    decision.notes
                };
                Some(SystemCommand::AnswerQuestion { ticket_id, answer })
            }
            "status_query" => Some(SystemCommand::StatusQuery),
            _ => Some(SystemCommand::GeneralMessage {
                text: msg.text.clone(),
            }),
        };

        cmd.map(|c| InterpretedCommand {
            command: c,
            source: msg.clone(),
            confidence: 0.7,
        })
    }
}

// ── Pure helper functions (extracted from NexusNode) ────────────────────────

pub fn extract_ticket_id(text: &str) -> Option<String> {
    let parts: Vec<&str> = text.split_whitespace().collect();
    for part in parts {
        let cleaned = part.trim_end_matches(':');
        let lower = cleaned.to_lowercase();
        if lower.starts_with("t-") {
            let num = lower.trim_start_matches("t-");
            return Some(format!("T-{}", num.to_uppercase()));
        }
        if lower.starts_with("ticket-") {
            let num = lower.trim_start_matches("ticket-");
            return Some(format!("T-{}", num.to_uppercase()));
        }
    }
    None
}

pub fn extract_worker_id(text: &str) -> Option<String> {
    let lower = text.to_lowercase();
    let parts: Vec<&str> = lower.split_whitespace().collect();
    for part in parts.iter().skip(1) {
        if !part.starts_with("t-") && !part.starts_with("ticket-") {
            return Some(normalize_worker_id(part));
        }
    }
    None
}

/// Normalize informal worker IDs like "forge1" → "forge-1".
pub fn normalize_worker_id(raw: &str) -> String {
    let lower = raw.to_lowercase();
    let mut result = String::new();
    let mut prev_was_digit = false;
    let mut prev_was_letter = false;
    for ch in lower.chars() {
        let is_digit = ch.is_ascii_digit();
        let is_letter = ch.is_ascii_alphabetic();
        if is_digit && prev_was_letter && !prev_was_digit {
            result.push('-');
        }
        result.push(ch);
        prev_was_digit = is_digit;
        prev_was_letter = is_letter;
    }
    result
}

/// Extract two worker IDs from a word slice.
pub fn extract_two_workers(parts: &[&str]) -> (String, String) {
    if parts.len() < 3 {
        return (String::new(), String::new());
    }
    let (from_worker, next_idx) = read_worker(parts, 1);
    let (to_worker, _) = read_worker(parts, next_idx);
    (from_worker, to_worker)
}

pub fn read_worker(parts: &[&str], start: usize) -> (String, usize) {
    if start >= parts.len() {
        return (String::new(), start);
    }
    let token = parts[start].to_lowercase();
    if token == "to" || token == "into" || token == "from" {
        return read_worker(parts, start + 1);
    }
    let normalized = normalize_worker_id(parts[start]);
    if normalized.contains('-') {
        return (normalized, start + 1);
    }
    if start + 1 < parts.len() && parts[start + 1].parse::<u64>().is_ok() {
        let joined = format!("{}{}", parts[start], parts[start + 1]);
        return (normalize_worker_id(&joined), start + 2);
    }
    (normalized, start + 1)
}
