// crates/agent-client/src/openai.rs
//
// OpenAiClient — calls the OpenAI Chat Completions API with tool support.
//
// Compatible with any OpenAI-compatible proxy (e.g. DeepSeek, OpenRouter, etc)
// via the OPENAI_API_URL environment variable.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::types::{ContentBlock, LlmClient, LlmResponse, Message, ToolSchema};

// ── Constants ─────────────────────────────────────────────────────────────

const DEFAULT_OPENAI_API_URL: &str = "https://api.openai.com/v1/chat/completions";
const DEFAULT_MAX_TOKENS: u32 = 4096;

// ── Client ────────────────────────────────────────────────────────────────

pub struct OpenAiClient {
    http: Client,
    api_key: String,
    api_url: String,
    pub model: String,
    max_tokens: u32,
}

impl OpenAiClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            api_url: DEFAULT_OPENAI_API_URL.to_string(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    /// Load API key from OPENAI_API_KEY env var.
    /// Uses gpt-4o-mini by default (fast + cheap for orchestration).
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("OPENAI_API_KEY").context("OPENAI_API_KEY not set")?;
        let model = std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
        // Support both OPENAI_BASE_URL (preferred) and OPENAI_API_URL (legacy)
        let api_url = if let Ok(base_url) = std::env::var("OPENAI_BASE_URL") {
            format!("{}/chat/completions", base_url.trim_end_matches('/'))
        } else if let Ok(api_url) = std::env::var("OPENAI_API_URL") {
            api_url
        } else {
            DEFAULT_OPENAI_API_URL.to_string()
        };
        Ok(Self {
            http: Client::new(),
            api_url,
            api_key: key,
            model,
            max_tokens: DEFAULT_MAX_TOKENS,
        })
    }

    /// Like `from_env()`, but overrides the model name.
    /// Used when the registry specifies a `model_backend` that maps to OpenAI-compatible.
    pub fn from_env_with_model(model_override: &str) -> Result<Self> {
        let mut client = Self::from_env()?;
        client.model = model_override.to_string();
        tracing::info!(model = %model_override, "OpenAiClient model overridden from registry");
        Ok(client)
    }

    /// Create an OpenAiClient configured to use a proxy endpoint.
    /// Routes through PROXY_URL as the API URL and uses PROXY_API_KEY for auth.
    /// Falls back to OPENAI_API_KEY if PROXY_API_KEY is not set.
    /// For self-hosted proxies without auth, uses a dummy key.
    pub fn from_proxy(model_override: &str) -> Result<Self> {
        let proxy_url = std::env::var("PROXY_URL")
            .or_else(|_| std::env::var("ANTHROPIC_BASE_URL"))
            .context("PROXY_URL not set — required for OpenAI-compatible proxy routing")?;
        let api_url = format!("{}/chat/completions", proxy_url.trim_end_matches('/'));
        // Priority: PROXY_API_KEY > OPENAI_API_KEY > dummy key for no-auth proxies
        let api_key = std::env::var("PROXY_API_KEY")
            .or_else(|_| std::env::var("OPENAI_API_KEY"))
            .unwrap_or_else(|_| "no-key".to_string());

        tracing::info!(
            api_url = %api_url,
            model = %model_override,
            has_auth = !api_key.is_empty() && api_key != "no-key",
            "OpenAiClient configured with proxy"
        );

        Ok(Self {
            http: Client::new(),
            api_url,
            api_key,
            model: model_override.to_string(),
            max_tokens: DEFAULT_MAX_TOKENS,
        })
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
}

// ── Serialization helpers ─────────────────────────────────────────────────

/// Converts our `Message` enum into the raw JSON format OpenAI expects.
fn messages_to_json(messages: &[Message]) -> Value {
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

// ── Main API call ─────────────────────────────────────────────────────────

#[async_trait]
impl LlmClient for OpenAiClient {
    async fn send(&self, messages: &[Message], tools: &[ToolSchema]) -> Result<LlmResponse> {
        let messages_json = messages_to_json(messages);

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
            .map_err(|e| {
                if e.is_connect() {
                    anyhow::anyhow!(
                        "Cannot connect to OpenAI API at {} — {}. Is the service running?",
                        self.api_url,
                        e
                    )
                } else {
                    anyhow::anyhow!(
                        "HTTP request to OpenAI API failed (url={}): {}",
                        self.api_url,
                        e
                    )
                }
            })?;

        let status = resp.status();
        let raw_text = resp
            .text()
            .await
            .context("Failed to read OpenAI response body")?;
        debug!(model = %self.model, status = %status, body = %raw_text, "← OpenAI raw response");

        let raw: Value = serde_json::from_str(&raw_text).context(format!(
            "Failed to parse OpenAI response (status={}, body={})",
            status,
            &raw_text[..raw_text.len().min(500)]
        ))?;

        if !status.is_success() {
            let error_msg = raw["error"]["message"].as_str().unwrap_or("unknown");
            bail!("OpenAI API error {}: {}", status, error_msg);
        }

        debug!(model = %self.model, "← OpenAI response");

        let choice = raw["choices"]
            .get(0)
            .context("OpenAI returned no choices")?;
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

    fn model(&self) -> &str {
        &self.model
    }
}
