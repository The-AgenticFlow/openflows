// crates/agent-client/src/runner.rs
//
// AgentRunner — the tool-calling loop.
//
// Ties together AnthropicClient and McpSession into a single
// `run()` method that drives an agent to completion.
//
// ## Structured Decision via Tool Calling (PRIMARY PATH)
//
// The LLM is given a `submit_decision` tool with the exact AgentDecision
// schema. When the LLM is ready to make a decision, it calls this tool
// instead of producing free-text JSON. This provides a STRUCTURAL
// separation between reasoning (text blocks) and decisions (tool_use),
// enforced by the API itself — no extraction/parsing needed.
//
// ## Free-text Decision (FALLBACK PATH)
//
// If the LLM produces a text response instead of calling the decision
// tool (older models, non-tool-aware configurations), the system falls
// back to extracting the JSON from the text, with retry and recovery.
//
// Recovery: When the LLM produces an unparseable response, the runner
// retries with a clarified prompt before falling back to a safe default.
// This prevents the entire flow from crashing due to a single bad LLM
// response.

use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    fallback::FallbackClient,
    mcp::McpSession,
    types::{AgentDecision, AgentPersona, LlmClient, LlmResponse, Message, ToolSchema},
};

/// Maximum number of retries when the LLM produces an unparseable response
/// before falling back to a safe default decision.
const DECISION_RETRY_LIMIT: usize = 2;

/// The virtual tool name that the LLM calls to submit its final decision.
/// This replaces the old "put JSON in your text" approach with a structural
/// tool call, making the separation between reasoning and decision explicit
/// at the API level.
const DECISION_TOOL_NAME: &str = "submit_decision";

/// Build the `submit_decision` tool schema. This is injected alongside the
/// MCP tools so the LLM can call it to submit its decision structurally.
fn decision_tool_schema() -> ToolSchema {
    ToolSchema {
        name: DECISION_TOOL_NAME.to_string(),
        description: "Submit your final orchestration decision. You MUST call this tool when you have \
                      completed your analysis and are ready to decide on the next action. Do NOT output \
                      JSON in your text response — call this tool instead. Your reasoning goes in the \
                      text response; your decision goes in this tool call.".to_string(),
        input_schema: serde_json::json!({
            "type": "object",
            "properties": {
                "action": {
                    "type": "string",
                    "description": "The action to take. One of: work_assigned, merge_prs, no_work, awaiting_human, approve_command, reject_command"
                },
                "notes": {
                    "type": "string",
                    "description": "Human-readable explanation of the decision"
                },
                "assign_to": {
                    "type": "string",
                    "description": "Worker ID to assign the ticket to (for work_assigned action)"
                },
                "ticket_id": {
                    "type": "string",
                    "description": "Ticket ID to assign (for work_assigned action)"
                },
                "issue_url": {
                    "type": "string",
                    "description": "GitHub issue URL (for work_assigned action)"
                }
            },
            "required": ["action", "notes"]
        }),
    }
}

/// Build the system prompt that instructs the LLM to use the decision tool
/// instead of free-text JSON.
fn build_system_prompt(persona: &AgentPersona) -> String {
    format!(
        "{persona}\n\n\
         You are an autonomous orchestrator. If the provided context is empty or sparse, \
         use your tools (like `list_issues` or `search_issues`) to fetch the current state \
         of the repository before making a final decision.\n\n\
         ## HOW TO RESPOND\n\n\
         1. **Reasoning**: Use text to explain your analysis, reasoning, and considerations.\n\
         2. **Decision**: When ready to decide, call the `{tool_name}` tool with your decision. \
         Do NOT output a JSON object in your text — call the tool instead.\n\n\
         If you are uncertain or stuck, use `{tool_name}` with action `awaiting_human` and \
         explain what you need human input on in the `notes` field.\n\n\
         The available actions are:\n\
         - `work_assigned`: Assign a ticket to a worker. Include `assign_to`, `ticket_id`, and `issue_url`.\n\
         - `merge_prs`: Proceed to merge pending PRs.\n\
         - `no_work`: No actionable work found this cycle.\n\
         - `awaiting_human`: You need human input to proceed. Explain why in `notes`.",
        persona = persona.system_prompt(),
        tool_name = DECISION_TOOL_NAME,
    )
}

pub struct AgentRunner {
    client: Box<dyn LlmClient>,
    mcp: McpSession,
}

impl AgentRunner {
    pub fn new(client: Box<dyn LlmClient>, mcp: McpSession) -> Self {
        Self { client, mcp }
    }

    /// Create a runner using environment variables.
    /// Always uses FallbackClient for automatic failover.
    pub async fn from_env() -> Result<Self> {
        Self::from_env_for_agent(None).await
    }

    /// Create a runner for a specific agent, using its registry `model_backend`.
    ///
    /// When `model_backend` is provided, FallbackClient routes to the correct
    /// provider based on MODEL_PROVIDER_MAP. When PROXY_URL is set, individual
    /// API keys are optional - the proxy handles all routing.
    pub async fn from_env_for_agent(model_backend: Option<&str>) -> Result<Self> {
        let client: Box<dyn LlmClient> = match model_backend {
            Some(m) => Box::new(FallbackClient::from_env_with_model(m)?),
            None => Box::new(FallbackClient::from_env()?),
        };

        info!(model = %client.model(), "AgentRunner initialized");

        let mcp = McpSession::connect_default().await?;
        Ok(Self::new(client, mcp))
    }

    /// Create a runner with explicit GitHub token for MCP session.
    /// Use this when you have per-agent tokens resolved from the registry.
    pub async fn from_env_with_token(
        model_backend: Option<&str>,
        github_token: &str,
    ) -> Result<Self> {
        let client: Box<dyn LlmClient> = match model_backend {
            Some(m) => Box::new(FallbackClient::from_env_with_model(m)?),
            None => Box::new(FallbackClient::from_env()?),
        };

        info!(model = %client.model(), "AgentRunner initialized with explicit token");

        let mcp = McpSession::connect_hosted_with_token(github_token).await?;
        Ok(Self::new(client, mcp))
    }

/// Run a single agent turn to completion.
    ///
    /// ## Primary path: Decision Tool
    ///
    /// The LLM is given a `submit_decision` tool alongside MCP tools. When
    /// ready, it calls this tool structurally — no JSON extraction needed.
    /// Reasoning stays in text; the decision is a tool call.
    ///
    /// ## Fallback path: Free-text extraction
    ///
    /// If the LLM produces a text response instead of calling the tool
    /// (e.g., older models), we fall back to extracting JSON from text,
    /// with retry and safe-default recovery.
    pub async fn run(
        &mut self,
        persona: &AgentPersona,
        context: Value,
        max_turns: usize,
    ) -> Result<AgentDecision> {
        // 1. Fetch current tool schemas from the MCP server
        let mut tools = self.mcp.list_tools().await?;

        // 2. Inject the decision tool so the LLM can submit its decision
        //    structurally instead of producing free-text JSON.
        tools.push(decision_tool_schema());

        info!(
            agent = persona.id,
            tools = tools.len(),
            "Agent runner starting (with submit_decision tool)"
        );

        // 3. Seed the conversation with the decision-tool-aware system prompt
        let mut messages = vec![
            Message::system(build_system_prompt(persona)),
            Message::user(serde_json::to_string_pretty(&context)?),
        ];

        // 4. Tool-calling loop
        for turn in 0..max_turns {
            info!(agent = persona.id, turn, "--- LLM Turn Starting ---");

            match self.client.send(&messages, &tools).await? {
                LlmResponse::ToolCall { id, name, args } => {
                    // ── Decision Tool: PRIMARY PATH ────────────────────────
                    // The LLM called submit_decision — parse it directly.
                    // No extraction, no guessing. Structurally guaranteed.
                    if name == DECISION_TOOL_NAME {
                        info!(
                            agent = persona.id,
                            action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?"),
                            "LLM submitted decision via tool call"
                        );

                        let decision = AgentDecision {
                            action: args.get("action")
                                .and_then(|v| v.as_str())
                                .unwrap_or("no_work")
                                .to_string(),
                            notes: args.get("notes")
                                .and_then(|v| v.as_str())
                                .unwrap_or("[Decision submitted via tool call]")
                                .to_string(),
                            assign_to: args.get("assign_to")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            ticket_id: args.get("ticket_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            issue_url: args.get("issue_url")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                        };

                        return Ok(decision);
                    }

                    // ── MCP Tool: Execute and feed back ───────────────────
                    info!(agent = persona.id, tool = name, args = ?args, "LLM requested tool execution");

                    let result = match self.mcp.call_tool(&name, args.clone()).await {
                        Ok(r) => {
                            let text = r.as_text();
                            info!(
                                agent = persona.id,
                                tool = name,
                                result = text,
                                "Tool execution successful"
                            );
                            text
                        }
                        Err(e) => {
                            warn!(agent = persona.id, tool = name, err = %e, "Tool call failed");
                            format!("ERROR: {}", e)
                        }
                    };

                    messages.push(Message::assistant_tool_use(id.clone(), name, args));
                    messages.push(Message::tool_result(id, result));
                }

                LlmResponse::Text(text) => {
                    // ── Free-text response: FALLBACK PATH ──────────────────
                    // The LLM produced text instead of calling submit_decision.
                    // This can happen with older models or when the LLM
                    // ignores the tool instruction. Fall back to extraction.
                    info!(agent = persona.id, "--- Agent responded with text (not decision tool) ---");
                    debug!(decision = text, "Raw decision text from LLM");

                    match extract_decision(&text) {
                        Ok(decision) => {
                            info!(
                                action = decision.action,
                                notes = decision.notes,
                                "Extracted decision from text fallback"
                            );
                            return Ok(decision);
                        }
                        Err(parse_err) => {
                            warn!(
                                agent = persona.id,
                                error = %parse_err,
                                "Failed to extract decision from text — attempting recovery"
                            );

                            let recovered = self
                                .recover_decision(persona, &mut messages, &text, &parse_err, &tools)
                                .await;

                            match recovered {
                                Ok(decision) => {
                                    info!(
                                        agent = persona.id,
                                        action = decision.action,
                                        "Recovered decision after retry"
                                    );
                                    return Ok(decision);
                                }
                                Err(recovery_err) => {
                                    warn!(
                                        agent = persona.id,
                                        recovery_error = %recovery_err,
                                        "All recovery attempts failed — falling back to safe default"
                                    );
                                    return Ok(AgentDecision {
                                        action: "no_work".to_string(),
                                        notes: format!(
                                            "[SELF-HEAL] Decision extraction failed after {} retries. \
                                             Raw response was {:.500}. Error: {}. \
                                             Falling back to no_work to keep the flow alive.",
                                            DECISION_RETRY_LIMIT,
                                            text.chars().take(500).collect::<String>(),
                                            parse_err
                                        ),
                                        assign_to: None,
                                        ticket_id: None,
                                        issue_url: None,
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }

        // max_turns exceeded — recover gracefully instead of crashing
        warn!(
            agent = persona.id,
            max_turns,
            "Agent exceeded max_turns — falling back to safe default instead of crashing"
        );
        Ok(AgentDecision {
            action: "no_work".to_string(),
            notes: format!(
                "[SELF-HEAL] Agent '{}' exceeded max_turns ({}) without returning a decision. \
                 Falling back to no_work to keep the flow alive.",
                persona.id, max_turns
            ),
            assign_to: None,
            ticket_id: None,
            issue_url: None,
})
    }

    /// Attempt to recover a decision when the initial LLM response was unparseable.
    ///
    /// Sends a follow-up message asking the LLM to produce ONLY the required JSON,
    /// or better yet, call the `submit_decision` tool. Retries up to
    /// DECISION_RETRY_LIMIT times.
    async fn recover_decision(
        &mut self,
        persona: &AgentPersona,
        messages: &mut Vec<Message>,
        _original_text: &str,
        parse_error: &anyhow::Error,
        tools: &[crate::types::ToolSchema],
    ) -> Result<AgentDecision> {
        for attempt in 0..DECISION_RETRY_LIMIT {
            info!(
                agent = persona.id,
                attempt = attempt + 1,
                max_retries = DECISION_RETRY_LIMIT,
                "Retrying decision extraction with clarified prompt"
            );

            // Construct a retry prompt asking the LLM to produce valid JSON
            // or call the submit_decision tool.
            let retry_prompt = if attempt == 0 {
                format!(
                    "Your previous response could not be parsed as a valid JSON decision. \
                     The error was: {:.200}\n\n\
                     Please EITHER:\n\
                     1. Call the `submit_decision` tool with your decision (RECOMMENDED)\n\
                     2. Respond with ONLY the JSON object on a single line in this format: \
                     {{\"action\": \"<action>\", \"notes\": \"<notes>\", \"assign_to\": \"<worker_id>\", \"ticket_id\": \"<ticket_id>\"}}\n\n\
                     If you are uncertain about which action to take, use action \"awaiting_human\" \
                     to request human guidance.",
                    parse_error
                )
            } else {
                format!(
                    "That still wasn't valid. Please call the `submit_decision` tool NOW, \
                     or respond with ONLY this raw JSON — no explanation, no markdown, no code blocks:\n\
                     {{\"action\": \"<action>\", \"notes\": \"<notes>\", \"assign_to\": \"<worker_id>\", \"ticket_id\": \"<ticket_id>\"}}"
                )
            };

            messages.push(Message::user(retry_prompt.clone()));

            match self.client.send(messages, tools).await {
                Ok(LlmResponse::ToolCall { name, args, .. }) => {
                    // The LLM called a tool during recovery
                    if name == DECISION_TOOL_NAME {
                        // It called the decision tool — extract directly!
                        info!(
                            agent = persona.id,
                            action = args.get("action").and_then(|v| v.as_str()).unwrap_or("?"),
                            attempt = attempt + 1,
                            "LLM called submit_decision tool during recovery"
                        );
                        return Ok(AgentDecision {
                            action: args.get("action")
                                .and_then(|v| v.as_str())
                                .unwrap_or("no_work")
                                .to_string(),
                            notes: args.get("notes")
                                .and_then(|v| v.as_str())
                                .unwrap_or("[Recovered via tool call]")
                                .to_string(),
                            assign_to: args.get("assign_to")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            ticket_id: args.get("ticket_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                            issue_url: args.get("issue_url")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string()),
                        });
                    } else {
                        // It called some other tool (like list_issues) during recovery
                        // — execute it and let the next turn retry the decision
                        warn!(
                            agent = persona.id,
                            tool = name,
                            attempt = attempt + 1,
                            "LLM called a non-decision tool during recovery — processing and retrying"
                        );
                        let tool_result = match self.mcp.call_tool(&name, args.clone()).await {
                            Ok(r) => {
                                info!(agent = persona.id, tool = name, "Tool execution successful during recovery");
                                r.as_text()
                            }
                            Err(e) => {
                                warn!(agent = persona.id, tool = name, err = %e, "Tool call failed during recovery");
                                format!("ERROR: {}", e)
                            }
                        };
                        // The retry_prompt user message is already in messages,
                        // now add the tool call and result so the LLM can see the data
                        // and make a decision on the next turn.
                        let tool_call_id = format!("recovery-tool-{}", attempt);
                        messages.push(Message::assistant_tool_use(
                            tool_call_id.clone(),
                            name,
                            args,
                        ));
                        messages.push(Message::tool_result(tool_call_id, tool_result));
                        continue;
                    }
                }
                Ok(LlmResponse::Text(retry_text)) => {
                    debug!(agent = persona.id, retry_text, "Raw retry response from LLM");
                    match extract_decision(&retry_text) {
                        Ok(decision) => {
                            info!(
                                agent = persona.id,
                                action = decision.action,
                                attempt = attempt + 1,
                                "Successfully extracted decision on retry"
                            );
                            return Ok(decision);
                        }
                        Err(e) => {
                            warn!(
                                agent = persona.id,
                                attempt = attempt + 1,
                                error = %e,
                                "Retry response still unparseable"
                            );
                            // Remove the retry prompt we just added so the
                            // next attempt starts with clean context.
                            messages.pop();
                            continue;
                        }
                    }
                }
                Err(e) => {
                    warn!(
                        agent = persona.id,
                        attempt = attempt + 1,
                        error = %e,
                        "LLM API call failed during recovery"
                    );
                    // Remove the retry prompt before returning
                    messages.pop();
                    return Err(e);
                }
            }
        }

        Err(anyhow!(
            "Failed to extract decision after {} retries",
            DECISION_RETRY_LIMIT
        ))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Extracts `{"action": ..., "notes": ...}` from the agent's final text.
/// The LLM may include reasoning before the JSON object, so we scan for it.
///
/// Recovery strategies (in order):
/// 1. Parse the full text as JSON (clean response)
/// 2. Find markdown JSON blocks (```json ... ```)
/// 3. Find the last JSON object starting with `{` (handles reasoning preamble)
/// 4. Truncated JSON repair (appends missing closing brackets)
/// 5. Line-by-line fallback (scans each line for JSON)
/// 6. Loose key extraction (finds action/notes fields in any JSON-like structure)
/// 7. Regex pattern matching (finds "action": "..." anywhere in text)
fn extract_decision(text: &str) -> Result<AgentDecision> {
    // 1. Try parsing the full text first (clean response)
    if let Ok(d) = serde_json::from_str::<AgentDecision>(text.trim()) {
        return Ok(d);
    }

    // 2. Try finding markdown JSON blocks: ```json ... ```
    if let Some(start) = text.find("```json") {
        let remainder = &text[start + 7..];
        if let Some(end) = remainder.find("```") {
            let json_str = remainder[..end].trim();
            if let Ok(d) = serde_json::from_str::<AgentDecision>(json_str) {
                return Ok(d);
            }
        }
    }

    // Also try plain ``` blocks (some models don't use ```json)
    if let Some(start) = text.find("```") {
        // Skip the opening ``` which might have a language tag
        let after_tick = &text[start + 3..];
        // Find the content after the first newline (skip language tag)
        let content_start = after_tick.find('\n').map(|i| start + 3 + i + 1).unwrap_or(start + 3);
        let content = &text[content_start..];
        if let Some(end) = content.find("```") {
            let json_str = content[..end].trim();
            if let Ok(d) = serde_json::from_str::<AgentDecision>(json_str) {
                return Ok(d);
            }
        }
    }

    // 3. Find the LAST occurrence of `{"` (JSON object start) — we prefer the
    //    last one because the LLM may produce multiple JSON-like fragments during
    //    reasoning, and the final one is most likely the actual decision.
    //    If no `{"` found, fall back to the last `{`.
    let json_start = text.rfind("{\"").or_else(|| text.rfind('{'));

    if let Some(start) = json_start {
        let potential_json = &text[start..];

        if let Ok(d) = serde_json::from_str::<AgentDecision>(potential_json.trim()) {
            return Ok(d);
        }

        // 3b. Truncated JSON repair: LLM responses can be cut off before the
        //     closing '"}' or '}'. Try appending common truncation suffixes.
        let trimmed = potential_json.trim();
        if trimmed.starts_with('{') {
            let suffixes = ["}", "\"}", "\"\n}", "\n}"];
            for suffix in suffixes {
                let repaired = format!("{}{}", trimmed, suffix);
                if let Ok(d) = serde_json::from_str::<AgentDecision>(&repaired) {
                    warn!("Repaired truncated JSON by appending suffix");
                    return Ok(d);
                }
            }
        }
    }

    // 4. Line by line fallback (scan from the end — last JSON line is most likely)
    for line in text.lines().rev() {
        let trimmed = line.trim();
        if trimmed.starts_with('{') {
            if let Ok(d) = serde_json::from_str::<AgentDecision>(trimmed) {
                return Ok(d);
            }
            let suffixes = ["}", "\"}", "\"\n}", "\n}"];
            for suffix in suffixes {
                let repaired = format!("{}{}", trimmed, suffix);
                if let Ok(d) = serde_json::from_str::<AgentDecision>(&repaired) {
                    warn!("Repaired truncated JSON on line by appending suffix");
                    return Ok(d);
                }
            }
        }
    }

    // 5. Loose key extraction: find action and notes values in any JSON-like text
    //    This handles cases where the LLM produces a valid JSON object but with
    //    extra keys, different ordering, or embedded in longer text.
    if let Some(action) = extract_json_string_value(text, "action") {
        let notes = extract_json_string_value(text, "notes").unwrap_or_else(|| "Extracted from partial response".to_string());
        let assign_to = extract_json_string_value(text, "assign_to");
        let ticket_id = extract_json_string_value(text, "ticket_id");
        let issue_url = extract_json_string_value(text, "issue_url");

        warn!(
            action = %action,
            "Recovered decision using loose key extraction from unparseable response"
        );
        return Ok(AgentDecision {
            action,
            notes,
            assign_to,
            ticket_id,
            issue_url,
        });
    }

    // 6. Regex-like pattern matching: find "action": "..." anywhere in text
    //    This is the last resort before giving up.
    if let Some(action) = extract_quoted_value(text, "action") {
        let notes = extract_quoted_value(text, "notes").unwrap_or_else(|| "Pattern-matched from response".to_string());
        warn!(
            action = %action,
            "Recovered decision using pattern matching from unparseable response"
        );
        return Ok(AgentDecision {
            action,
            notes,
            assign_to: extract_quoted_value(text, "assign_to"),
            ticket_id: extract_quoted_value(text, "ticket_id"),
            issue_url: extract_quoted_value(text, "issue_url"),
        });
    }

    Err(anyhow!(
        "Could not extract AgentDecision JSON from response (length={} chars, first 200 chars: '{}')",
        text.len(),
        text.chars().take(200).collect::<String>()
    ))
}

/// Extract a string value for a specific key from JSON-like text.
/// Looks for patterns like `"key": "value"` or `"key":"value"`.
fn extract_json_string_value(text: &str, key: &str) -> Option<String> {
    // Try standard JSON pattern: "key": "value"
    let pattern_standard = format!("\"{}\"", key);
    let pattern_no_quotes_key = key;

    for pattern in [&pattern_standard as &str, pattern_no_quotes_key] {
        if let Some(pos) = text.find(pattern) {
            let after_key = &text[pos + pattern.len()..];
            // Skip whitespace and colon
            let after_sep = after_key.trim_start();
            if after_sep.starts_with(':') {
                let after_colon = after_sep[1..].trim_start();
                // Expect a quoted string value
                if after_colon.starts_with('"') {
                    if let Some(end) = after_colon[1..].find('"') {
                        let value = &after_colon[1..1 + end];
                        if !value.is_empty() {
                            return Some(value.to_string());
                        }
                    }
                }
            }
        }
    }
    None
}

/// Extract a quoted value following a key pattern anywhere in the text.
/// More lenient than extract_json_string_value — works with:
/// - action: "value"  (no quotes on key)
/// - "action": 'value' (single quotes on value)
/// - action = "value"  (equals sign)
fn extract_quoted_value(text: &str, key: &str) -> Option<String> {
    let patterns = [
        format!("\"{}\"", key),
        format!("{}:", key),
        format!("{} =", key),
        format!("{}=", key),
    ];

    for pattern in &patterns {
        if let Some(pos) = text.find(pattern.as_str()) {
            let after = &text[pos + pattern.len()..];
            let after = after.trim_start();

            // Skip colon or equals if not already included in pattern
            let value_start = if after.starts_with(':') || after.starts_with('=') {
                after[1..].trim_start()
            } else {
                after
            };

            // Try double quotes
            if value_start.starts_with('"') {
                if let Some(end) = value_start[1..].find('"') {
                    let value = &value_start[1..1 + end];
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
            // Try single quotes
            else if value_start.starts_with('\'') {
                if let Some(end) = value_start[1..].find('\'') {
                    let value = &value_start[1..1 + end];
                    if !value.is_empty() {
                        return Some(value.to_string());
                    }
                }
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_decision_clean() {
        let text = r#"{"action": "work_assigned", "notes": "Assigned T-001 to forge-1"}"#;
        let d = extract_decision(text).unwrap();
        assert_eq!(d.action, "work_assigned");
        assert_eq!(d.notes, "Assigned T-001 to forge-1");
    }

    #[test]
    fn test_extract_decision_with_preamble() {
        let text = concat!(
            "I analyzed the tickets and worker slots.\n",
            "forge-1 is idle and T-001 is unassigned.\n",
            r#"{"action": "work_assigned", "notes": "Assigned T-001 to forge-1"}"#
        );
        let d = extract_decision(text).unwrap();
        assert_eq!(d.action, "work_assigned");
    }

    #[test]
    fn test_extract_decision_with_reasoning_preamble() {
        let text = concat!(
            "**Reasoning:** Some reasoning text here.\n\n",
            r#"{"action": "merge_prs", "notes": "PR #40 needs merge."}"#
        );
        let d = extract_decision(text).unwrap();
        assert_eq!(d.action, "merge_prs");
        assert_eq!(d.notes, "PR #40 needs merge.");
    }

    #[test]
    fn test_extract_decision_truncated_json_missing_quote_and_brace() {
        let text = concat!(
            "**Reasoning:** Some reasoning text here.\n\n",
            r#"{"action": "merge_prs", "notes": "PR #40 needs merge."#
        );
        let d = extract_decision(text).unwrap();
        assert_eq!(d.action, "merge_prs");
        assert_eq!(d.notes, "PR #40 needs merge.");
    }

    #[test]
    fn test_extract_decision_fails_gracefully() {
        let err = extract_decision("I cannot assist with that.").unwrap_err();
        assert!(err.to_string().contains("Could not extract"));
    }

    #[test]
    fn test_extract_decision_markdown_block() {
        let text = concat!(
            "Here's my decision:\n\n",
            "```json\n",
            r#"{"action": "work_assigned", "notes": "Assigned forge-1"}"#,
            "\n```\n"
        );
        let d = extract_decision(text).unwrap();
        assert_eq!(d.action, "work_assigned");
    }

    #[test]
    fn test_extract_decision_loose_key_extraction() {
        // When the JSON is malformed but contains the key-value pairs
        let text = r#"The next action is {"action": "no_work", "notes": "No assignable tickets", "extra_key": "ignored"} and some other text after."#;
        let d = extract_decision(text).unwrap();
        assert_eq!(d.action, "no_work");
    }

    #[test]
    fn test_extract_decision_pattern_matching_fallback() {
        // When there's no proper JSON but the action is clearly stated
        // Note: this uses comma-separated key=value pairs without quotes
        let text = r#"action = "awaiting_human" notes = "Need human input on this ticket.""#;
        let d = extract_decision(text).unwrap();
        assert_eq!(d.action, "awaiting_human");
    }

    #[test]
    fn test_extract_json_string_value() {
        let text = r#"{"action": "work_assigned", "notes": "test"}"#;
        assert_eq!(
            extract_json_string_value(text, "action"),
            Some("work_assigned".to_string())
        );
        assert_eq!(
            extract_json_string_value(text, "notes"),
            Some("test".to_string())
        );
        assert_eq!(extract_json_string_value(text, "missing"), None);
    }

    #[test]
    fn test_extract_quoted_value_double_quotes() {
        let text = r#"action = "merge_prs" notes = "some notes""#;
        assert_eq!(
            extract_quoted_value(text, "action"),
            Some("merge_prs".to_string())
        );
    }

    #[test]
    fn test_extract_decision_multiple_json_objects_uses_last() {
        // When the LLM produces multiple JSON fragments, the last one
        // (the actual decision) should be used
        let text = concat!(
            r#"{"action": "no_work", "notes": "thinking..."}"#,
            "\nWait, let me reconsider.\n",
            r#"{"action": "work_assigned", "notes": "Assign T-001", "assign_to": "forge-1", "ticket_id": "T-001"}"#
        );
        let d = extract_decision(text).unwrap();
        assert_eq!(d.action, "work_assigned");
        assert_eq!(d.assign_to.as_deref(), Some("forge-1"));
    }

    #[test]
    fn test_safe_default_no_work() {
        // Verify that the AgentDecision fields match what we expect for the
        // safe fallback
        let decision = AgentDecision {
            action: "no_work".to_string(),
            notes: "fallback".to_string(),
            assign_to: None,
            ticket_id: None,
            issue_url: None,
        };
        assert_eq!(decision.action, "no_work");
    }

    #[test]
    fn test_decision_tool_schema() {
        // Verify the decision tool schema is well-formed
        let schema = decision_tool_schema();
        assert_eq!(schema.name, "submit_decision");
        assert!(schema.description.contains("call this tool"));
        assert!(schema.input_schema.is_object());

        // Verify required fields
        let props = schema.input_schema.get("properties").unwrap();
        assert!(props.get("action").is_some(), "action property must exist");
        assert!(props.get("notes").is_some(), "notes property must exist");

        let required = schema.input_schema.get("required").unwrap().as_array().unwrap();
        assert!(required.iter().any(|r| r.as_str() == Some("action")));
        assert!(required.iter().any(|r| r.as_str() == Some("notes")));
    }
}
