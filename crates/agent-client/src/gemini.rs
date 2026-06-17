// crates/agent-client/src/gemini.rs
//
// GeminiClient — calls the Gemini generateContent API with function calling.

use anyhow::{bail, Context, Result};
use async_trait::async_trait;
use reqwest::Client;
use serde_json::{json, Map, Value};
use tracing::debug;

use crate::types::{ContentBlock, LlmClient, LlmResponse, Message, ToolSchema};

const DEFAULT_GEMINI_API_BASE: &str = "https://generativelanguage.googleapis.com/v1beta";
const DEFAULT_MAX_OUTPUT_TOKENS: u32 = 4096;

pub struct GeminiClient {
    http: Client,
    api_key: String,
    pub model: String,
    max_output_tokens: u32,
}

impl GeminiClient {
    pub fn new(api_key: impl Into<String>, model: impl Into<String>) -> Self {
        Self {
            http: Client::new(),
            api_key: api_key.into(),
            model: normalize_model_name(&model.into()),
            max_output_tokens: DEFAULT_MAX_OUTPUT_TOKENS,
        }
    }

    /// Load API key from GEMINI_API_KEY env var.
    /// Uses gemini-2.5-flash by default for orchestration.
    pub fn from_env() -> Result<Self> {
        let key = std::env::var("GEMINI_API_KEY").context("GEMINI_API_KEY not set")?;
        let model =
            std::env::var("GEMINI_MODEL").unwrap_or_else(|_| "gemini-2.5-flash".to_string());
        Ok(Self::new(key, model))
    }

    pub fn with_max_output_tokens(mut self, n: u32) -> Self {
        self.max_output_tokens = n;
        self
    }

    fn endpoint(&self) -> String {
        if let Ok(url) = std::env::var("GEMINI_API_URL") {
            return url;
        }

        let base = std::env::var("GEMINI_API_BASE")
            .unwrap_or_else(|_| DEFAULT_GEMINI_API_BASE.to_string());
        format!(
            "{}/models/{}:generateContent?key={}",
            base.trim_end_matches('/'),
            self.model,
            self.api_key
        )
    }
}

fn normalize_model_name(model: &str) -> String {
    let stripped = crate::strip_provider_prefix(model);
    stripped.trim().trim_start_matches("models/").to_ascii_lowercase().to_string()
}

fn validate_model_name(model: &str) -> Result<()> {
    if model.is_empty() {
        bail!("GEMINI_MODEL must not be empty");
    }

    if model.contains(' ') {
        bail!(
            "GEMINI_MODEL must be a Gemini API model code such as `gemini-2.5-flash-lite`, not a display name like `Gemini 2.5 Flash-Lite`"
        );
    }

    if !model.starts_with("gemini-") {
        bail!(
            "GEMINI_MODEL must start with `gemini-` (for example `gemini-2.5-flash` or `gemini-2.5-flash-lite`)"
        );
    }

    if !model
        .chars()
        .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-' || ch == '.')
    {
        bail!(
            "GEMINI_MODEL contains unsupported characters; use a Gemini API model code such as `gemini-2.5-flash-lite`"
        );
    }

    Ok(())
}

fn tool_name_for_id(messages: &[Message], tool_use_id: &str) -> Option<String> {
    for message in messages.iter().rev() {
        if let Message::Assistant { content } = message {
            for block in content {
                if let ContentBlock::ToolUse { id, name, .. } = block {
                    if id == tool_use_id {
                        return Some(name.clone());
                    }
                }
            }
        }
    }

    None
}

fn parse_tool_result_payload(content: &str) -> Value {
    let trimmed = content.trim();

    match serde_json::from_str(trimmed) {
        Ok(Value::Object(map)) => Value::Object(map),
        Ok(other) => json!({ "output": other }),
        Err(_) if trimmed.starts_with("ERROR:") => json!({ "error": trimmed }),
        Err(_) => json!({ "output": trimmed }),
    }
}

fn messages_to_gemini_json(messages: &[Message]) -> Result<(Option<Value>, Value)> {
    let mut system_instruction = None;
    let mut turns = Vec::new();

    for message in messages {
        match message {
            Message::System { content } => {
                system_instruction = Some(json!({
                    "parts": [{ "text": content }]
                }));
            }
            Message::User { content } => {
                turns.push(json!({
                    "role": "user",
                    "parts": [{ "text": content }],
                }));
            }
            Message::Assistant { content } => {
                let mut parts = Vec::new();

                for block in content {
                    match block {
                        ContentBlock::Text { text } => {
                            parts.push(json!({ "text": text }));
                        }
                        ContentBlock::ToolUse { name, input, .. } => {
                            parts.push(json!({
                                "functionCall": {
                                    "name": name,
                                    "args": input,
                                }
                            }));
                        }
                    }
                }

                if !parts.is_empty() {
                    turns.push(json!({
                        "role": "model",
                        "parts": parts,
                    }));
                }
            }
            Message::ToolResult {
                tool_use_id,
                content,
            } => {
                let name = tool_name_for_id(messages, tool_use_id)
                    .context("Could not map Gemini tool result to tool name")?;
                turns.push(json!({
                    "role": "user",
                    "parts": [{
                        "functionResponse": {
                            "id": tool_use_id,
                            "name": name,
                            "response": parse_tool_result_payload(content),
                        }
                    }],
                }));
            }
        }
    }

    Ok((system_instruction, json!(turns)))
}

fn json_schema_to_gemini(schema: &Value) -> Value {
    match schema {
        Value::Object(map) => {
            let mut normalized = Map::new();
            let mut required: Option<&Value> = None;

            for (key, value) in map {
                match key.as_str() {
                    // Gemini only accepts a subset of JSON Schema fields here.
                    "type" => {
                        let next_value = match value.as_str() {
                            Some(kind) => Value::String(kind.to_uppercase()),
                            None => json_schema_to_gemini(value),
                        };
                        normalized.insert("type".to_string(), next_value);
                    }
                    "description" | "format" | "nullable" => {
                        normalized.insert(key.clone(), value.clone());
                    }
                    "enum" | "items" => {
                        normalized.insert(key.clone(), json_schema_to_gemini(value));
                    }
                    "properties" => {
                        normalized.insert("properties".to_string(), properties_to_gemini(value));
                    }
                    "required" => {
                        required = Some(value);
                    }
                    // Drop unsupported JSON Schema keywords such as:
                    // $schema, additionalProperties, default, title, examples, oneOf, anyOf, etc.
                    _ => {}
                }
            }

            if let Some(required) = required {
                if let Some(properties) = normalized.get("properties").and_then(Value::as_object) {
                    let valid_required: Vec<Value> = required
                        .as_array()
                        .into_iter()
                        .flatten()
                        .filter_map(Value::as_str)
                        .filter(|name| properties.contains_key(*name))
                        .map(|name| Value::String(name.to_string()))
                        .collect();

                    if !valid_required.is_empty() {
                        normalized.insert("required".to_string(), Value::Array(valid_required));
                    }
                }
            }

            Value::Object(normalized)
        }
        Value::Array(items) => Value::Array(items.iter().map(json_schema_to_gemini).collect()),
        other => other.clone(),
    }
}

fn properties_to_gemini(properties: &Value) -> Value {
    match properties {
        Value::Object(map) => {
            let normalized = map
                .iter()
                .map(|(name, schema)| (name.clone(), json_schema_to_gemini(schema)))
                .collect::<Map<String, Value>>();
            Value::Object(normalized)
        }
        _ => Value::Object(Map::new()),
    }
}

#[async_trait]
impl LlmClient for GeminiClient {
    async fn send(&self, messages: &[Message], tools: &[ToolSchema]) -> Result<LlmResponse> {
        validate_model_name(&self.model)?;

        let (system_instruction, contents) = messages_to_gemini_json(messages)?;

        let mut body = json!({
            "contents": contents,
            "generationConfig": {
                "maxOutputTokens": self.max_output_tokens,
            }
        });

        if let Some(system_instruction) = system_instruction {
            body["system_instruction"] = system_instruction;
        }

        if !tools.is_empty() {
            let declarations: Vec<Value> = tools
                .iter()
                .map(|tool| {
                    json!({
                        "name": tool.name,
                        "description": tool.description,
                        "parameters": json_schema_to_gemini(&tool.input_schema),
                    })
                })
                .collect();

            body["tools"] = json!([{
                "function_declarations": declarations
            }]);
            body["tool_config"] = json!({
                "function_calling_config": {
                    "mode": "AUTO"
                }
            });
        }

        let resp = self
            .http
            .post(self.endpoint())
            .header("Content-Type", "application/json")
            .json(&body)
            .send()
            .await
            .context("HTTP request to Gemini API failed")?;

        let status = resp.status();
        let raw: Value = resp
            .json()
            .await
            .context("Failed to parse Gemini response")?;

        if !status.is_success() {
            let error_msg = raw["error"]["message"].as_str().unwrap_or("unknown");
            bail!("Gemini API error {}: {}", status, error_msg);
        }

        debug!(model = %self.model, "← Gemini response");

        let candidate = raw["candidates"]
            .get(0)
            .context("Gemini returned no candidates")?;
        let parts = candidate["content"]["parts"]
            .as_array()
            .context("Gemini returned no content parts")?;

        for (index, part) in parts.iter().enumerate() {
            if let Some(call) = part.get("functionCall") {
                let name = call["name"].as_str().unwrap_or("").to_string();
                let args = call["args"].clone();
                let id = raw["responseId"]
                    .as_str()
                    .map(|response_id| format!("{}-{}", response_id, index))
                    .unwrap_or_else(|| format!("gemini-tool-{}", index));

                return Ok(LlmResponse::ToolCall { id, name, args });
            }
        }

        let text = parts
            .iter()
            .filter_map(|part| part["text"].as_str())
            .collect::<Vec<_>>()
            .join("\n");

        Ok(LlmResponse::Text(text))
    }

    fn model(&self) -> &str {
        &self.model
    }
}

#[cfg(test)]
mod tests {
    use super::{
        messages_to_gemini_json, normalize_model_name, parse_tool_result_payload,
        validate_model_name,
    };
    use crate::types::Message;
    use serde_json::json;

    #[test]
    fn normalizes_models_prefix_and_case() {
        assert_eq!(
            normalize_model_name("models/Gemini-2.5-Flash-Lite"),
            "gemini-2.5-flash-lite"
        );
    }

    #[test]
    fn rejects_display_name_with_spaces() {
        let err = validate_model_name("Gemini 3.1 Flash-Lite").unwrap_err();
        assert!(err.to_string().contains("must be a Gemini API model code"));
    }

    #[test]
    fn accepts_api_model_code() {
        validate_model_name("gemini-2.5-flash-lite").unwrap();
    }

    #[test]
    fn wraps_array_tool_results_in_output_object() {
        assert_eq!(
            parse_tool_result_payload(r#"[{"id":1},{"id":2}]"#),
            json!({
                "output": [
                    { "id": 1 },
                    { "id": 2 }
                ]
            })
        );
    }

    #[test]
    fn preserves_object_tool_results() {
        assert_eq!(
            parse_tool_result_payload(r#"{"result":"ok"}"#),
            json!({ "result": "ok" })
        );
    }

    #[test]
    fn tool_results_include_matching_function_response_id() {
        let messages = vec![
            Message::user("Find issues"),
            Message::assistant_tool_use("call-123", "list_issues", json!({})),
            Message::tool_result("call-123", r#"[{"number":1}]"#),
        ];

        let (_, contents) = messages_to_gemini_json(&messages).unwrap();
        assert_eq!(
            contents[2]["parts"][0]["functionResponse"],
            json!({
                "id": "call-123",
                "name": "list_issues",
                "response": {
                    "output": [
                        { "number": 1 }
                    ]
                }
            })
        );
    }
}
