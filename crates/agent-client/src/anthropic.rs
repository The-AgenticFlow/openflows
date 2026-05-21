// crates/agent-client/src/anthropic.rs
//
// AnthropicClient — calls the Anthropic Messages API with tool_use support.
//
// Uses the reqwest HTTP client. No SDK dependency needed — the API is simple
// enough to call directly and avoids version pinning issues.

use anyhow::{bail, Context, Result};
use reqwest::Client;

use serde_json::{json, Value};
use tracing::debug;

use crate::types::{ContentBlock, LlmClient, LlmResponse, Message, ToolSchema};

// ── Constants ─────────────────────────────────────────────────────────────

const DEFAULT_ANTHROPIC_API_URL: &str = "https://api.anthropic.com/v1/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

// ── Client ────────────────────────────────────────────────────────────────

pub struct AnthropicClient {
    http: Client,
    api_key: String,
    api_url: String,
    pub model: String,
    max_tokens: u32,
}

impl AnthropicClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            api_key: api_key.into(),
            api_url: DEFAULT_ANTHROPIC_API_URL.to_string(),
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    fn resolve_api_url() -> String {
        if let Ok(url) = std::env::var("ANTHROPIC_API_URL") {
            return url;
        }

        let base = std::env::var("ANTHROPIC_BASE_URL")
            .ok()
            .or_else(|| std::env::var("PROXY_URL").ok());

        if let Some(base) = base {
            return format!("{}/messages", base.trim_end_matches('/'));
        }

        DEFAULT_ANTHROPIC_API_URL.to_string()
    }

    fn resolve_api_key(proxy_active: bool) -> Result<String> {
        if proxy_active {
            if let Ok(key) = std::env::var("PROXY_API_KEY") {
                return Ok(key);
            }
            if let Ok(key) = std::env::var("ANTHROPIC_API_KEY") {
                return Ok(key);
            }
            return Ok("no-key".to_string());
        }
        std::env::var("ANTHROPIC_API_KEY").context("ANTHROPIC_API_KEY not set")
    }

    /// Load API key from ANTHROPIC_API_KEY env var.
    /// Uses claude-3-5-haiku by default (fast + cheap for orchestration).
    ///
    /// When PROXY_URL or ANTHROPIC_BASE_URL is set, routes requests through
    /// the proxy and uses PROXY_API_KEY for authentication (falling back to
    /// ANTHROPIC_API_KEY if PROXY_API_KEY is not set).
    pub fn from_env() -> Result<Self> {
        let api_url = Self::resolve_api_url();
        let proxy_active =
            std::env::var("PROXY_URL").is_ok() || std::env::var("ANTHROPIC_BASE_URL").is_ok();
        let api_key = Self::resolve_api_key(proxy_active)?;
        let model = std::env::var("ANTHROPIC_MODEL")
            .unwrap_or_else(|_| "claude-3-5-haiku-20241022".to_string());

        if proxy_active {
            tracing::info!(
                api_url = %api_url,
                model = %model,
                "AnthropicClient configured with proxy"
            );
        }

        Ok(Self {
            http: Client::new(),
            api_key,
            api_url,
            model,
            max_tokens: DEFAULT_MAX_TOKENS,
        })
    }

    pub fn from_env_with_model(model_override: &str) -> Result<Self> {
        let mut client = Self::from_env()?;
        client.model = model_override.to_string();
        tracing::info!(model = %model_override, "AnthropicClient model overridden from registry");
        Ok(client)
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
}

// ── Serialization helpers ─────────────────────────────────────────────────

/// Converts our `Message` enum into the raw JSON format Anthropic expects.
fn messages_to_json(messages: &[Message]) -> Value {
    let mut system_prompt = String::new();
    let mut turns: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            Message::System { content } => {
                system_prompt = content.clone();
            }
            Message::User { content } => {
                turns.push(json!({ "role": "user", "content": content }));
            }
            Message::Assistant { content } => {
                let blocks: Vec<Value> = content
                    .iter()
                    .map(|b| match b {
                        ContentBlock::Text { text } => json!({ "type": "text", "text": text }),
                        ContentBlock::ToolUse { id, name, input } => json!({
                            "type":  "tool_use",
                            "id":    id,
                            "name":  name,
                            "input": input,
                        }),
                    })
                    .collect();
                turns.push(json!({ "role": "assistant", "content": blocks }));
            }
            Message::ToolResult {
                tool_use_id,
                content,
            } => {
                turns.push(json!({
                    "role": "user",
                    "content": [{
                        "type":        "tool_result",
                        "tool_use_id": tool_use_id,
                        "content":     content,
                    }]
                }));
            }
        }
    }

    json!({ "system": system_prompt, "messages": turns })
}

// ── Main API call ─────────────────────────────────────────────────────────

#[async_trait::async_trait]
impl LlmClient for AnthropicClient {
    /// Send messages to the API. Returns exactly one `LlmResponse`.
    async fn send(&self, messages: &[Message], tools: &[ToolSchema]) -> Result<LlmResponse> {
        let msg_json = messages_to_json(messages);
        let tools_json: Vec<Value> = tools
            .iter()
            .map(|t| {
                json!({
                    "name":         t.name,
                    "description":  t.description,
                    "input_schema": t.input_schema,
                })
            })
            .collect();

        let body = json!({
            "model":      self.model,
            "max_tokens": self.max_tokens,
            "system":     msg_json["system"],
            "messages":   msg_json["messages"],
            "tools":      tools_json,
        });

        let resp = self
            .http
            .post(&self.api_url)
            .header("x-api-key", &self.api_key)
            .header("anthropic-version", ANTHROPIC_VERSION)
            .header("content-type", "application/json")
            .json(&body)
            .send()
            .await
            .context("HTTP request to Anthropic API failed")?;

        let status = resp.status();
        let raw_text = resp
            .text()
            .await
            .context("Failed to read Anthropic response body")?;
        let truncation_len = raw_text.len().min(500);
        let truncation_len = raw_text
            .char_indices()
            .take_while(|(i, _)| *i <= truncation_len)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        debug!(stop_reason = %raw_text.len(), status = %status, body = %&raw_text[..truncation_len], "← Anthropic raw response");

        let raw: Value = serde_json::from_str(&raw_text).context(format!(
            "Failed to parse Anthropic response (status={}, body={})",
            status,
            &raw_text[..truncation_len]
        ))?;

        if !status.is_success() {
            bail!(
                "Anthropic API error {}: {}",
                status,
                raw["error"]["message"].as_str().unwrap_or("unknown")
            );
        }

        debug!(
            stop_reason = raw["stop_reason"].as_str(),
            "← Anthropic response"
        );

        let content = &raw["content"];
        let blocks = content
            .as_array()
            .context("Anthropic returned no content array")?;

        let stop_reason = raw["stop_reason"].as_str().unwrap_or("");

        if stop_reason == "tool_use" {
            for block in blocks {
                if block["type"].as_str() == Some("tool_use") {
                    return Ok(LlmResponse::ToolCall {
                        id: block["id"].as_str().unwrap_or("").to_string(),
                        name: block["name"].as_str().unwrap_or("").to_string(),
                        args: block["input"].clone(),
                    });
                }
            }
            bail!("Anthropic stop_reason was tool_use but no tool_use block found");
        }

        let text = blocks
            .iter()
            .filter_map(|b| {
                if b["type"].as_str() == Some("text") {
                    b["text"].as_str()
                } else {
                    None
                }
            })
            .collect::<Vec<_>>()
            .join("\n");

        Ok(LlmResponse::Text(text))
    }

    fn model(&self) -> &str {
        &self.model
    }
}
