use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Value};
use tracing::debug;

use crate::types::{ContentBlock, LlmClient, LlmResponse, Message, ToolSchema};

const FIREWORKS_API_URL: &str = "https://api.fireworks.ai/inference/v1/chat/completions";
const DEFAULT_MAX_TOKENS: u32 = 4096;

pub struct FireworksClient {
    http: Client,
    api_key: String,
    api_url: String,
    pub model: String,
    max_tokens: u32,
}

impl FireworksClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            api_url: FIREWORKS_API_URL.to_string(),
            api_key: api_key.into(),
            model: model.into(),
            max_tokens: DEFAULT_MAX_TOKENS,
        }
    }

    pub fn from_env() -> Result<Self> {
        let key = std::env::var("FIREWORKS_API_KEY").context("FIREWORKS_API_KEY not set")?;
        let model = std::env::var("FIREWORKS_MODEL")
            .unwrap_or_else(|_| "accounts/fireworks/models/llama-v3p1-8b-instruct".to_string());
        let api_url =
            std::env::var("FIREWORKS_API_URL").unwrap_or_else(|_| FIREWORKS_API_URL.to_string());
        Ok(Self {
            http: Client::new(),
            api_url,
            api_key: key,
            model,
            max_tokens: DEFAULT_MAX_TOKENS,
        })
    }

    pub fn from_env_with_model(model_override: &str) -> Result<Self> {
        let mut client = Self::from_env()?;
        client.model = model_override.to_string();
        tracing::info!(model = %model_override, "FireworksClient model overridden");
        Ok(client)
    }

    pub fn is_configured() -> bool {
        std::env::var("FIREWORKS_API_KEY").is_ok()
    }

    pub fn with_max_tokens(mut self, n: u32) -> Self {
        self.max_tokens = n;
        self
    }
}

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

#[async_trait]
impl LlmClient for FireworksClient {
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
                        "Cannot connect to Fireworks API at {} — {}. Is the service running?",
                        self.api_url,
                        e
                    )
                } else {
                    anyhow::anyhow!(
                        "HTTP request to Fireworks API failed (url={}): {}",
                        self.api_url,
                        e
                    )
                }
            })?;

        let status = resp.status();
        let raw_text = resp
            .text()
            .await
            .context("Failed to read Fireworks response body")?;
        debug!(model = %self.model, status = %status, body = %raw_text, "← Fireworks raw response");

        let raw: Value = serde_json::from_str(&raw_text).context(format!(
            "Failed to parse Fireworks response (status={}, body={})",
            status,
            &raw_text[..raw_text.len().min(500)]
        ))?;

        if !status.is_success() {
            let error_msg = raw["error"]["message"].as_str().unwrap_or("unknown");
            bail!("Fireworks API error {}: {}", status, error_msg);
        }

        debug!(model = %self.model, "← Fireworks response");

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

    fn model(&self) -> &str {
        &self.model
    }
}
