// crates/agent-client/src/fireworks.rs
//
// FireworksClient — supports both OpenAI and Anthropic API formats.
//
// By default uses Anthropic format for Claude CLI compatibility.
// Set FIREWORKS_API_FORMAT=openai to use OpenAI format.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::types::{ContentBlock, LlmClient, LlmResponse, Message, ToolSchema};

const FIREWORKS_OPENAI_URL: &str = "https://api.fireworks.ai/inference/v1/chat/completions";
const FIREWORKS_ANTHROPIC_URL: &str = "https://api.fireworks.ai/inference/v1/anthropic/messages";
const ANTHROPIC_VERSION: &str = "2023-06-01";
const DEFAULT_MAX_TOKENS: u32 = 4096;

/// API format for Fireworks requests.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FireworksApiFormat {
    /// OpenAI-compatible format (chat/completions endpoint)
    OpenAi,
    /// Anthropic-compatible format (anthropic/messages endpoint)
    Anthropic,
}

impl Default for FireworksApiFormat {
    fn default() -> Self {
        // Default to OpenAI since Fireworks doesn't support Anthropic endpoint directly
        Self::OpenAi
    }
}

pub struct FireworksClient {
    http: Client,
    api_key: String,
    api_url: String,
    pub model: String,
    max_tokens: u32,
    api_format: FireworksApiFormat,
}

impl FireworksClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            api_url: FIREWORKS_ANTHROPIC_URL.to_string(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
            api_format: FireworksApiFormat::Anthropic,
        }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("FIREWORKS_API_KEY").context("FIREWORKS_API_KEY not set")?;
        let model = std::env::var("FIREWORKS_MODEL")
            .unwrap_or_else(|_| "accounts/fireworks/models/llama-v3p1-8b-instruct".to_string());
        
        // Fireworks only supports OpenAI format (no native Anthropic endpoint)
        // Default to OpenAI, ignore FIREWORKS_API_FORMAT env var
        let api_format = FireworksApiFormat::OpenAi;

        // Determine API URL based on format
        let api_url = std::env::var("FIREWORKS_API_URL").unwrap_or_else(|_| {
            FIREWORKS_OPENAI_URL.to_string()
        });

        tracing::info!(
            api_format = ?api_format,
            api_url = %api_url,
            model = %model,
            "FireworksClient initialized"
        );

        Ok(Self {
            http: Client::new(),
            api_url,
            api_key: key,
            model,
            max_tokens: DEFAULT_MAX_TOKENS,
            api_format,
        })
    }

    pub fn from_env_with_model(model_override: &str) -> Result<Self> {
        let mut client = Self::from_env()?;
        client.model = model_override.to_string();
        tracing::info!(model = %model_override, api_format = ?client.api_format, "FireworksClient model overridden");
        Ok(client)
    }

    pub fn is_configured() -> bool {
        std::env::var("FIREWORKS_API_KEY").is_ok()
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }

    pub fn api_format(&self) -> FireworksApiFormat {
        self.api_format
    }
}

// ── OpenAI format message serialization ──────────────────────────────────────

fn messages_to_openai_json(messages: &[Message]) -> Value {
    let mut turns: Vec<Value> = Vec::new();

    for msg in messages {
        match msg {
            Message::System { content } => {
                turns.push(json!({ "role": "system", "content": content }));
            }
            Message::User { content } => {
                turns.push(json!({ "role": "user", "content": content }));
            }
            Message::Assistant { content } => {
                let mut text_content = String::new();
                let mut tool_calls = Vec::new();

                for block in content {
                    match block {
                        ContentBlock::Text { text } => {
                            if !text_content.is_empty() {
                                text_content.push('\n');
                            }
                            text_content.push_str(text);
                        }
                        ContentBlock::ToolUse { id, name, input } => {
                            tool_calls.push(json!({
                                "id":   id,
                                "type": "function",
                                "function": {
                                    "name":      name,
                                    "arguments": input.to_string(),
                                }
                            }));
                        }
                    }
                }

                let mut assistant_msg = json!({ "role": "assistant" });
                if !text_content.is_empty() {
                    assistant_msg["content"] = json!(text_content);
                } else {
                    assistant_msg["content"] = Value::Null;
                }

                if !tool_calls.is_empty() {
                    assistant_msg["tool_calls"] = json!(tool_calls);
                }

                turns.push(assistant_msg);
            }
            Message::ToolResult {
                tool_use_id,
                content,
            } => {
                turns.push(json!({
                    "role":         "tool",
                    "tool_call_id": tool_use_id,
                    "content":      content,
                }));
            }
        }
    }

    json!(turns)
}

// ── Anthropic format message serialization ────────────────────────────────────

fn messages_to_anthropic_json(messages: &[Message]) -> Value {
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

// ── Main API call ─────────────────────────────────────────────────────────────

#[async_trait]
impl LlmClient for FireworksClient {
    async fn send(&self, messages: &[Message], tools: &[ToolSchema]) -> Result<LlmResponse> {
        match self.api_format {
            FireworksApiFormat::OpenAi => self.send_openai(messages, tools).await,
            FireworksApiFormat::Anthropic => self.send_anthropic(messages, tools).await,
        }
    }

    fn model(&self) -> &str {
        &self.model
    }
}

impl FireworksClient {
    async fn send_openai(&self, messages: &[Message], tools: &[ToolSchema]) -> Result<LlmResponse> {
        let messages_json = messages_to_openai_json(messages);

        let mut body = json!({
            "model":      self.model,
            "max_tokens": self.max_tokens,
            "messages":   messages_json,
        });

        if !tools.is_empty() {
            let tools_json: Vec<Value> = tools
                .iter()
                .map(|t| {
                    json!({
                        "type": "function",
                        "function": {
                            "name":        t.name,
                            "description": t.description,
                            "parameters":  t.input_schema,
                        }
                    })
                })
                .collect();
            body["tools"] = json!(tools_json);
        }

        let resp = self
            .http
            .post(&self.api_url)
            .header("Authorization", format!("Bearer {}", self.api_key))
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("HTTP request to Fireworks API failed")?;

        let status = resp.status();
        let raw_text = resp
            .text()
            .await
            .context("Failed to read Fireworks response body")?;
        debug!(model = %self.model, status = %status, body = %&raw_text[..raw_text.len().min(500)], "← Fireworks OpenAI raw response");

        let raw: Value = serde_json::from_str(&raw_text).context(format!(
            "Failed to parse Fireworks response (status={}, body={})",
            status,
            &raw_text[..raw_text.len().min(500)]
        ))?;

        if !status.is_success() {
            let error_msg = raw["error"]["message"].as_str().unwrap_or("unknown");
            bail!("Fireworks API error {}: {}", status, error_msg);
        }

        let choice = raw["choices"]
            .get(0)
            .context("Fireworks returned no choices")?;
        let message = &choice["message"];

        if let Some(tool_calls) = message["tool_calls"].as_array() {
            if let Some(tool_call) = tool_calls.first() {
                let id = tool_call["id"].as_str().unwrap_or("").to_string();
                let name = tool_call["function"]["name"]
                    .as_str()
                    .unwrap_or("")
                    .to_string();
                let args_str = tool_call["function"]["arguments"].as_str().unwrap_or("{}");
                let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

                return Ok(LlmResponse::ToolCall { id, name, args });
            }
        }

        let text = message["content"].as_str().unwrap_or("").to_string();
        Ok(LlmResponse::Text(text))
    }

    async fn send_anthropic(&self, messages: &[Message], tools: &[ToolSchema]) -> Result<LlmResponse> {
        let msg_json = messages_to_anthropic_json(messages);

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
            .context("HTTP request to Fireworks Anthropic API failed")?;

        let status = resp.status();
        let raw_text = resp
            .text()
            .await
            .context("Failed to read Fireworks Anthropic response body")?;
        
        let truncation_len = raw_text.len().min(500);
        let truncation_len = raw_text
            .char_indices()
            .take_while(|(i, _)| *i <= truncation_len)
            .last()
            .map(|(i, c)| i + c.len_utf8())
            .unwrap_or(0);
        debug!(stop_reason = %raw_text.len(), status = %status, body = %&raw_text[..truncation_len], "← Fireworks Anthropic raw response");

        let raw: Value = serde_json::from_str(&raw_text).context(format!(
            "Failed to parse Fireworks Anthropic response (status={}, body={})",
            status,
            &raw_text[..truncation_len]
        ))?;

        if !status.is_success() {
            bail!(
                "Fireworks Anthropic API error {}: {}",
                status,
                raw["error"]["message"].as_str().unwrap_or("unknown")
            );
        }

        debug!(
            stop_reason = raw["stop_reason"].as_str(),
            "← Fireworks Anthropic response"
        );

        let content = &raw["content"];
        let blocks = content
            .as_array()
            .context("Fireworks Anthropic returned no content array")?;

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
            bail!("Fireworks Anthropic stop_reason was tool_use but no tool_use block found");
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
}
