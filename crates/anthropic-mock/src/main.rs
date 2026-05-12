use axum::{
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use futures::StreamExt;
use reqwest::Client;
use serde::Deserialize;
use serde_json::{json, Value};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use tokio::sync::RwLock;
use tracing::{debug, error, info, warn};

#[derive(Debug, Deserialize)]
struct AnthropicRequest {
    model: String,
    messages: Vec<Value>,
    system: Option<Value>,
    tools: Option<Vec<Value>>,
    max_tokens: Option<u32>,
    _stream: Option<bool>,
}

fn resolve_backend_url() -> String {
    let url = std::env::var("GATEWAY_URL")
        .or_else(|_| std::env::var("PROXY_URL"))
        .or_else(|_| std::env::var("OPENAI_API_URL"))
        .unwrap_or_else(|_| "https://api.openai.com/v1".to_string());
    format!("{}/chat/completions", url.trim_end_matches('/'))
}

fn resolve_backend_key() -> Result<String, String> {
    std::env::var("GATEWAY_API_KEY")
        .or_else(|_| std::env::var("FIREWORKS_API_KEY"))
        .or_else(|_| std::env::var("PROXY_API_KEY"))
        .or_else(|_| std::env::var("OPENAI_API_KEY"))
        .map_err(|_| {
            "GATEWAY_API_KEY, FIREWORKS_API_KEY, PROXY_API_KEY, or OPENAI_API_KEY must be set"
                .to_string()
        })
}

fn parse_model_map() -> HashMap<String, String> {
    let raw = std::env::var("MODEL_MAP").unwrap_or_default();
    if raw.is_empty() {
        return HashMap::new();
    }
    raw.split(',')
        .filter_map(|entry| {
            let mut parts = entry.splitn(2, '=');
            let from = parts.next()?.trim().to_string();
            let to = parts.next()?.trim().to_string();
            if !from.is_empty() && !to.is_empty() {
                Some((from, to))
            } else {
                None
            }
        })
        .collect()
}

#[tokio::main]
async fn main() {
    match dotenvy::dotenv() {
        Ok(path) => eprintln!("Loaded environment from {}", path.display()),
        Err(dotenvy::Error::Io(err)) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => panic!("Failed to load .env: {}", err),
    }
    tracing_subscriber::fmt::init();

    let backend_url = resolve_backend_url();
    let backend_key = resolve_backend_key().expect("API key configuration error");

    let model_map = parse_model_map();
    if !model_map.is_empty() {
        info!(
            mappings = model_map.len(),
            "Model name mapping loaded from MODEL_MAP"
        );
        for (from, to) in &model_map {
            info!(from = %from, to = %to, "Model map entry");
        }
    } else {
        info!("No MODEL_MAP configured - forwarding model names unchanged");
    }
    let model_map = Arc::new(RwLock::new(model_map));

    info!(
        backend_url = %backend_url,
        "Anthropic-to-OpenAI Proxy starting"
    );

    let backend_url_clone = backend_url.clone();
    let backend_key_clone = backend_key.clone();
    let app = Router::new()
        .route(
            "/v1/messages",
            post(move |headers, payload| {
                let url = backend_url_clone.clone();
                let key = backend_key_clone.clone();
                let map = model_map.clone();
                handle_messages(headers, payload, url, key, map)
            }),
        )
        .route("/v1/models", axum::routing::get(handle_models))
        .route("/health", axum::routing::get(|| async { "ok" }));

    let port = std::env::var("PORT").unwrap_or_else(|_| "8080".to_string());
    let addr = format!("0.0.0.0:{}", port).parse::<SocketAddr>().unwrap();

    info!("Proxy listening on {} - forward to {}", addr, backend_url);
    let listener = tokio::net::TcpListener::bind(addr).await.unwrap();
    axum::serve(listener, app).await.unwrap();
}

fn convert_anthropic_to_openai_messages(payload: &AnthropicRequest) -> Vec<Value> {
    let mut openai_messages = Vec::new();

    if let Some(sys) = &payload.system {
        match sys {
            Value::String(s) => {
                openai_messages.push(json!({ "role": "system", "content": s }));
            }
            Value::Array(arr) => {
                let text_parts: Vec<&str> = arr.iter().filter_map(|b| b["text"].as_str()).collect();
                if !text_parts.is_empty() {
                    openai_messages
                        .push(json!({ "role": "system", "content": text_parts.join("\n") }));
                }
            }
            _ => {
                openai_messages.push(json!({ "role": "system", "content": sys.to_string() }));
            }
        }
    }

    for msg in &payload.messages {
        let role = msg["role"].as_str().unwrap_or("user");
        let content = &msg["content"];

        if role == "assistant" && content.is_array() {
            let mut tool_calls = Vec::new();
            let mut text_content = String::new();

            for block in content.as_array().unwrap() {
                match block["type"].as_str() {
                    Some("tool_use") => {
                        tool_calls.push(json!({
                            "id": block["id"],
                            "type": "function",
                            "function": {
                                "name": block["name"],
                                "arguments": block["input"].to_string()
                            }
                        }));
                    }
                    Some("text") => {
                        text_content.push_str(block["text"].as_str().unwrap_or(""));
                    }
                    Some("thinking") => {
                        if let Some(t) = block["thinking"].as_str() {
                            text_content.push_str(t);
                        }
                    }
                    _ => {}
                }
            }

            let mut mapped_msg = json!({ "role": "assistant" });
            if !text_content.is_empty() {
                mapped_msg["content"] = json!(text_content);
            } else {
                mapped_msg["content"] = Value::Null;
            }
            if !tool_calls.is_empty() {
                mapped_msg["tool_calls"] = json!(tool_calls);
            }
            openai_messages.push(mapped_msg);
        } else if role == "user" && content.is_array() {
            let mut has_text = false;
            let mut text_parts = Vec::new();
            for block in content.as_array().unwrap() {
                if block["type"].as_str() == Some("tool_result") {
                    openai_messages.push(json!({
                        "role": "tool",
                        "tool_call_id": block["tool_use_id"],
                        "content": block["content"].as_str().unwrap_or("")
                    }));
                } else if block["type"].as_str() == Some("text") {
                    text_parts.push(block["text"].as_str().unwrap_or(""));
                    has_text = true;
                }
            }
            if has_text {
                openai_messages.push(json!({ "role": "user", "content": text_parts.join("\n") }));
            }
        } else {
            openai_messages.push(msg.clone());
        }
    }
    openai_messages
}

/// Handle GET /v1/models — return a list of Claude models so that
/// Claude Code's model validation succeeds when using the proxy.
///
/// Without this endpoint, Claude Code calls /v1/models to check whether
/// the resolved model name (e.g. `claude-sonnet-4-5`) is available.
/// A 404 response causes Claude Code to reject the model and exit
/// instantly with "There's an issue with the selected model".
async fn handle_models() -> Json<Value> {
    // All Claude model names that Claude Code may resolve aliases to.
    // The proxy maps any Claude model to PROXY_TARGET_MODEL, so all of
    // these are "available" from the proxy's perspective.
    let model_ids = [
        // Sonnet family — the `sonnet` alias resolves here
        "claude-sonnet-4-5",
        "claude-sonnet-4-5-20250514",
        "claude-sonnet-4-6",
        "claude-sonnet-4-6-20251022",
        "claude-3-5-sonnet-20241022",
        "claude-3-5-sonnet-latest",
        // Haiku family
        "claude-haiku-4-5",
        "claude-haiku-4-5-20251022",
        "claude-3-5-haiku-20241022",
        "claude-3-5-haiku-latest",
        // Opus family
        "claude-opus-4",
        "claude-opus-4-20250514",
        "claude-3-opus-20240229",
        "claude-3-opus-latest",
    ];

    let data: Vec<Value> = model_ids
        .iter()
        .map(|id| {
            json!({
                "id": id,
                "type": "model",
                "display_name": id.replace('-', " "),
                "created_at": "2024-01-01T00:00:00Z",
                "max_input_tokens": 200_000,
                "max_tokens": 8192
            })
        })
        .collect();

    let first = model_ids.first().unwrap_or(&"");
    let last = model_ids.last().unwrap_or(&"");

    Json(json!({
        "data": data,
        "has_more": false,
        "first_id": first,
        "last_id": last,
    }))
}

async fn handle_messages(
    _headers: HeaderMap,
    Json(payload): Json<AnthropicRequest>,
    backend_url: String,
    backend_key: String,
    model_map: Arc<RwLock<HashMap<String, String>>>,
) -> (StatusCode, Json<Value>) {
    let resolved_model = {
        let map = model_map.read().await;
        map.get(&payload.model)
            .cloned()
            .unwrap_or_else(|| payload.model.clone())
    };
    if resolved_model != payload.model {
        info!(requested = %payload.model, resolved = %resolved_model, "Model name mapped");
    }
    info!(
        turns = payload.messages.len(),
        model = %resolved_model,
        "Received Anthropic request"
    );

    let openai_messages = convert_anthropic_to_openai_messages(&payload);

    let openai_tools: Option<Vec<Value>> = payload.tools.clone().map(|tools| {
        tools
            .into_iter()
            .map(|t| {
                json!({
                    "type": "function",
                    "function": {
                        "name": t["name"],
                        "description": t["description"],
                        "parameters": t["input_schema"]
                    }
                })
            })
            .collect()
    });

    let mut openai_payload = json!({
        "model": resolved_model,
        "messages": openai_messages,
        "max_tokens": payload.max_tokens.unwrap_or(4096),
        "stream": true,
    });
    if let Some(tools) = openai_tools {
        openai_payload["tools"] = json!(tools);
    }

    debug!(openai_payload = %openai_payload, "Forwarding to backend with streaming");

    let client = Client::new();
    let resp = match client
        .post(&backend_url)
        .bearer_auth(&backend_key)
        .header("Content-Type", "application/json")
        .json(&openai_payload)
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            error!(err = %e, "Backend request failed");
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                Json(
                    json!({"type": "error", "error": {"type": "api_error", "message": format!("Backend request failed: {}", e)}}),
                ),
            );
        }
    };

    let status = resp.status();
    if !status.is_success() {
        let raw_text = resp.text().await.unwrap_or_default();
        warn!(status = %status, body = %&raw_text[..raw_text.len().min(500)], "Backend error response");
        let openai_raw: Value = serde_json::from_str(&raw_text).unwrap_or(json!({}));
        let err_msg = openai_raw["error"]["message"]
            .as_str()
            .unwrap_or("unknown error");
        return (
            StatusCode::from_u16(status.as_u16()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR),
            Json(json!({"type": "error", "error": {"type": "api_error", "message": err_msg}})),
        );
    }

    // Read SSE stream and reassemble
    let mut response_id = String::new();
    let mut text_content = String::new();
    let mut tool_calls: Vec<Value> = Vec::new();
    let mut finish_reason: Option<String> = None;
    let mut prompt_tokens: u64 = 0;
    let mut completion_tokens: u64 = 0;

    let mut stream = resp.bytes_stream();
    let mut buffer = String::new();

    while let Some(chunk_result) = stream.next().await {
        match chunk_result {
            Ok(bytes) => {
                let chunk_str = String::from_utf8_lossy(&bytes);
                buffer.push_str(&chunk_str);

                while let Some(pos) = buffer.find("\n\n") {
                    let event_data = buffer[..pos].to_string();
                    buffer = buffer[pos + 2..].to_string();

                    for line in event_data.lines() {
                        if let Some(data) = line.strip_prefix("data: ") {
                            if data == "[DONE]" {
                                continue;
                            }
                            if let Ok(chunk) = serde_json::from_str::<Value>(data) {
                                if response_id.is_empty() {
                                    response_id =
                                        chunk["id"].as_str().unwrap_or("unknown").to_string();
                                }

                                if let Some(usage) = chunk.get("usage") {
                                    prompt_tokens = usage["prompt_tokens"].as_u64().unwrap_or(0);
                                    completion_tokens =
                                        usage["completion_tokens"].as_u64().unwrap_or(0);
                                }

                                if let Some(choice) = chunk["choices"].get(0) {
                                    if let Some(fr) = choice["finish_reason"].as_str() {
                                        finish_reason = Some(fr.to_string());
                                    }
                                    if let Some(delta) = choice.get("delta") {
                                        if let Some(text) = delta["content"].as_str() {
                                            text_content.push_str(text);
                                        }
                                        if let Some(tc_array) = delta.get("tool_calls") {
                                            if let Some(arr) = tc_array.as_array() {
                                                for tc in arr {
                                                    let idx =
                                                        tc["index"].as_u64().unwrap_or(0) as usize;
                                                    while tool_calls.len() <= idx {
                                                        tool_calls.push(json!({
                                                            "id": "",
                                                            "type": "function",
                                                            "function": {"name": "", "arguments": ""}
                                                        }));
                                                    }
                                                    if let Some(id) = tc["id"].as_str() {
                                                        tool_calls[idx]["id"] = json!(id);
                                                    }
                                                    if let Some(func) = tc.get("function") {
                                                        if let Some(name) = func["name"].as_str() {
                                                            tool_calls[idx]["function"]["name"] =
                                                                json!(name);
                                                        }
                                                        if let Some(args) =
                                                            func["arguments"].as_str()
                                                        {
                                                            let existing = tool_calls[idx]
                                                                ["function"]["arguments"]
                                                                .as_str()
                                                                .unwrap_or("");
                                                            tool_calls[idx]["function"]
                                                                ["arguments"] = json!(format!(
                                                                "{}{}",
                                                                existing, args
                                                            ));
                                                        }
                                                    }
                                                }
                                            }
                                        }
                                    }
                                }
                            }
                        }
                    }
                }
            }
            Err(e) => {
                error!(err = %e, "Error reading stream chunk");
            }
        }
    }

    // Build Anthropic response
    let mut anthropic_content = Vec::new();

    if !text_content.is_empty() {
        anthropic_content.push(json!({ "type": "text", "text": text_content }));
    }

    for tc in &tool_calls {
        if tc["id"].as_str().map(|s| !s.is_empty()).unwrap_or(false) {
            let func = &tc["function"];
            let name = func["name"].as_str().unwrap_or("");
            let args_str = func["arguments"].as_str().unwrap_or("{}");
            let args: Value = serde_json::from_str(args_str).unwrap_or(json!({}));

            anthropic_content.push(json!({
                "type": "tool_use",
                "id": tc["id"],
                "name": name,
                "input": args
            }));
        }
    }

    if anthropic_content.is_empty() {
        anthropic_content.push(json!({ "type": "text", "text": "" }));
    }

    let stop_reason = match finish_reason.as_deref() {
        Some("tool_calls") => "tool_use",
        Some("stop") => "end_turn",
        Some("length") => "max_tokens",
        _ => "end_turn",
    };

    let anthropic_resp = json!({
        "id": format!("msg-{}", response_id),
        "type": "message",
        "role": "assistant",
        "model": payload.model,
        "content": anthropic_content,
        "stop_reason": stop_reason,
        "stop_sequence": Value::Null,
        "usage": {
            "input_tokens": prompt_tokens,
            "output_tokens": completion_tokens
        }
    });

    info!(stop_reason = stop_reason, "Returning Anthropic response");
    (StatusCode::OK, Json(anthropic_resp))
}
