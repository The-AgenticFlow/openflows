// crates/agent-client/src/runner.rs
//
// AgentRunner — the tool-calling loop.
//
// Ties together AnthropicClient and McpSession into a single
// `run()` method that drives an agent to completion.

use anyhow::{anyhow, Result};
use serde_json::Value;
use tracing::{debug, info, warn};

use crate::{
    fallback::FallbackClient,
    mcp::McpSession,
    types::{AgentDecision, AgentPersona, LlmClient, LlmResponse, Message},
};

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
    /// 1. Fetches available tool schemas from the MCP server.
    /// 2. Calls the Anthropic API with the persona's system prompt and context.
    /// 3. Executes tool calls via MCP until the LLM returns a final text response.
    /// 4. Parses the final response as `AgentDecision` JSON.
    ///
    /// The system prompt should instruct the LLM to return ONLY a JSON object:
    /// `{"action": "<action_string>", "notes": "<free_text>"}`
    pub async fn run(
        &mut self,
        persona: &AgentPersona,
        context: Value,
        max_turns: usize,
    ) -> Result<AgentDecision> {
        // 1. Fetch current tool schemas from the MCP server
        let tools = self.mcp.list_tools().await?;
        info!(
            agent = persona.id,
            tools = tools.len(),
            "Agent runner starting"
        );

        // 2. Seed the conversation
        let mut messages = vec![
            Message::system(format!(
                "{}\n\nYou are an autonomous orchestrator. \
                 If the provided context is empty or sparse, use your tools (like `list_issues` or `search_issues`) \
                 to fetch the current state of the repository before making a final decision. \
                 \n\nYou MUST end your final response with a JSON object on its own line: \
                 {{\"action\": \"<action>\", \"notes\": \"<notes>\", \"assign_to\": \"<worker_id>\", \"ticket_id\": \"<ticket_id>\"}}",
                persona.system_prompt()
            )),
            Message::user(serde_json::to_string_pretty(&context)?),
        ];

        // 3. Tool-calling loop
        for turn in 0..max_turns {
            info!(agent = persona.id, turn, "--- LLM Turn Starting ---");

            match self.client.send(&messages, &tools).await? {
                LlmResponse::ToolCall { id, name, args } => {
                    info!(agent = persona.id, tool = name, args = ?args, "LLM requested tool execution");

                    // Execute the tool via MCP
                    let result = match self.mcp.call_tool(&name, args.clone()).await {
                        Ok(r) => {
                            let text = r.as_text();
                            let truncated = truncate_tool_result(&text);
                            info!(
                                agent = persona.id,
                                tool = name,
                                original_len = text.len(),
                                truncated_len = truncated.len(),
                                "Tool execution successful"
                            );
                            truncated
                        }
                        Err(e) => {
                            warn!(agent = persona.id, tool = name, err = %e, "Tool call failed");
                            format!("ERROR: {}", e)
                        }
                    };

                    // Feed the result back into the conversation
                    messages.push(Message::assistant_tool_use(id.clone(), name, args));
                    messages.push(Message::tool_result(id, result));
                }

                LlmResponse::Text(text) => {
                    info!(agent = persona.id, "--- Agent reached final decision ---");
                    debug!(decision = text, "Raw decision text from LLM");

                    // Extract the JSON decision from the last line of the response
                    let decision = extract_decision(&text)?;
                    info!(
                        action = decision.action,
                        notes = decision.notes,
                        "Parsed decision"
                    );
                    return Ok(decision);
                }
            }
        }

        Err(anyhow!(
            "Agent '{}' exceeded max_turns ({}) without returning a decision",
            persona.id,
            max_turns
        ))
    }
}

// ── Helpers ───────────────────────────────────────────────────────────────

/// Maximum characters for a tool result before truncation.
/// GitHub API responses for issues/PRs can be extremely large (each issue
/// contains ~2KB of user metadata, URLs, reactions, etc.). When the LLM
/// receives these in full, it often runs out of context budget and produces
/// unstructured reasoning instead of the required JSON decision. This limit
/// ensures the context stays manageable.
const MAX_TOOL_RESULT_CHARS: usize = 16_000;

/// Truncates tool result text that exceeds MAX_TOOL_RESULT_CHARS.
///
/// For JSON arrays (like GitHub issue lists), this summarizes each item
/// to only the essential fields. For non-JSON or small responses, returns
/// the text unchanged.
fn truncate_tool_result(text: &str) -> String {
    if text.len() <= MAX_TOOL_RESULT_CHARS {
        return text.to_string();
    }

    // Try to parse as JSON array and slim down each item
    if let Ok(arr) = serde_json::from_str::<Vec<serde_json::Value>>(text) {
        let slimmed: Vec<serde_json::Value> = arr
            .into_iter()
            .map(|item| slim_issue_object(&item))
            .collect();

        if let Ok(slimmed_json) = serde_json::to_string_pretty(&slimmed) {
            if slimmed_json.len() <= MAX_TOOL_RESULT_CHARS {
                // Add a note that the response was summarized
                return format!(
                    "[Note: This response was truncated from {} bytes to save context. Key fields preserved.]\n\n{}",
                    text.len(),
                    slimmed_json
                );
            }
            // If even slimmed JSON is too large, truncate to just issue numbers and titles
            let minimal: Vec<serde_json::Value> = slimmed
                .into_iter()
                .map(|item| {
                    let number = item.get("number").cloned().unwrap_or(serde_json::Value::Null);
                    let title = item.get("title").cloned().unwrap_or(serde_json::Value::Null);
                    let status = item.get("status").cloned().unwrap_or(serde_json::Value::Null);
                    serde_json::json!({
                        "number": number,
                        "title": title,
                        "status": status,
                    })
                })
                .collect();

            return format!(
                "[Note: This response was heavily truncated from {} bytes. Only number, title, and status preserved.]\n\n{}",
                text.len(),
                serde_json::to_string_pretty(&minimal).unwrap_or_else(|_| format!("{:?}", minimal))
            );
        }
    }

    // For non-JSON responses, just truncate with a note.
    // Use char-boundary-safe truncation to avoid panicking on multi-byte UTF-8.
    let truncated: String = text.chars().take(MAX_TOOL_RESULT_CHARS).collect();
    format!(
        "[Note: Response truncated from {} bytes to {} bytes.]\n\n{}",
        text.len(),
        truncated.len(),
        truncated
    )
}

/// Reduce a GitHub issue/PR JSON object to its essential fields,
/// stripping verbose metadata like user URLs, reactions, timestamps, etc.
fn slim_issue_object(item: &serde_json::Value) -> serde_json::Value {
    let obj = match item.as_object() {
        Some(o) => o,
        None => return item.clone(),
    };

    // Essential fields to keep for GitHub issues
    let keep_keys = [
        "number",
        "title",
        "state",
        "body",
        "html_url",
        "labels",
        "assignees",
        "milestone",
        "status",
        "ticket_id",
        "worker_id",
        "priority",
        "branch",
        "attempts",
        "outcome",
        "action",
        "notes",
    ];

    let mut slim = serde_json::Map::new();
    for key in &keep_keys {
        if let Some(val) = obj.get(*key) {
            // Further slim down nested objects
            let slimmed_val = match key {
                &"labels" => slim_labels(val),
                &"assignees" => slim_assignees(val),
                &"milestone" => slim_milestone(val),
                &"body" => {
                    // Truncate body text to reasonable length.
                    // Use chars() to avoid panicking on multi-byte UTF-8.
                    if let Some(s) = val.as_str() {
                        if s.len() > 2000 {
                            let truncated_body: String = s.chars().take(2000).collect();
                            serde_json::Value::String(format!(
                                "{}\n\n[...truncated from {} chars]",
                                truncated_body,
                                s.chars().count()
                            ))
                        } else {
                            val.clone()
                        }
                    } else {
                        val.clone()
                    }
                }
                _ => val.clone(),
            };
            slim.insert(key.to_string(), slimmed_val);
        }
    }

    serde_json::Value::Object(slim)
}

fn slim_labels(val: &serde_json::Value) -> serde_json::Value {
    let arr = match val.as_array() {
        Some(a) => a,
        None => return val.clone(),
    };

    let slimmed: Vec<serde_json::Value> = arr
        .iter()
        .filter_map(|label| {
            let obj = label.as_object()?;
            Some(serde_json::json!({
                "name": obj.get("name"),
                "color": obj.get("color"),
            }))
        })
        .collect();

    serde_json::Value::Array(slimmed)
}

fn slim_assignees(val: &serde_json::Value) -> serde_json::Value {
    let arr = match val.as_array() {
        Some(a) => a,
        None => return val.clone(),
    };

    let slimmed: Vec<serde_json::Value> = arr
        .iter()
        .filter_map(|user| {
            let obj = user.as_object()?;
            Some(serde_json::json!({
                "login": obj.get("login"),
            }))
        })
        .collect();

    serde_json::Value::Array(slimmed)
}

fn slim_milestone(val: &serde_json::Value) -> serde_json::Value {
    let obj = match val.as_object() {
        Some(o) => o,
        None => return val.clone(),
    };

    serde_json::json!({
        "title": obj.get("title"),
        "number": obj.get("number"),
    })
}

/// Extracts `{"action": ..., "notes": ...}` from the agent's final text.
/// The LLM may include reasoning before the JSON object, so we scan for it.
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

    // 3. Find the start of a JSON object. We prefer `{"` (JSON object start)
    //    over any stray '{' in reasoning text, then fall back to rfind.
    let json_start = text.find("{\"").or_else(|| text.rfind('{'));

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

    // 4. Line by line fallback (original logic)
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

    Err(anyhow!(
        "Could not extract AgentDecision JSON from response:\n{}",
        text
    ))
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
}
