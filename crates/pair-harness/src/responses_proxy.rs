// crates/pair-harness/src/responses_proxy.rs
//! Lightweight HTTP proxy that translates OpenAI Responses API requests
//! to Chat Completions API requests for gateways that don't support
//! the Responses API (`/v1/responses`).
//!
//! This is a **special-case adapter** — it is ONLY started when the upstream
//! gateway returns 404 for `/v1/responses` but supports `/v1/chat/completions`.
//! Gateways that natively support `/v1/responses` (e.g., api.openai.com, Fireworks)
//! do NOT use this proxy.
//!
//! Architecture:
//!   Codex → localhost:PORT/v1/responses → [proxy translates] → upstream/v1/chat/completions
//!
//! The proxy handles:
//! - POST /v1/responses → translates request, streams response back as Responses API SSE
//! - GET /v1/models → passes through to upstream
//! - Real-time SSE streaming (translates Chat Completions chunks to Responses API deltas)

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::net::SocketAddr;
use std::sync::Arc;
use tracing::{debug, error, info, warn};
use uuid::Uuid;

// ---------------------------------------------------------------------------
// Pending function call tracking for streaming responses
// ---------------------------------------------------------------------------

/// Tracks a function call that is being streamed from the Chat Completions API.
/// Arguments arrive in incremental chunks and are accumulated here until the
/// stream ends, at which point we emit `function_call_arguments.done` and
/// include the completed function call in the `response.completed` output.
struct PendingFnCall {
    name: String,
    arguments: String,
}

// ---------------------------------------------------------------------------
// Request / Response types
// ---------------------------------------------------------------------------

/// Subset of the OpenAI Responses API request body we need to translate.
#[derive(Debug, Serialize, Deserialize)]
struct ResponsesApiRequest {
    model: String,
    #[serde(default)]
    input: Value,
    #[serde(default)]
    stream: Option<bool>,
    #[serde(default)]
    temperature: Option<f64>,
    #[serde(default)]
    max_output_tokens: Option<u64>,
    #[serde(default)]
    tools: Option<Value>,
    #[serde(default)]
    instructions: Option<String>,
    // Allow unknown fields to pass through
    #[serde(flatten)]
    extra: Value,
}

/// OpenAI Chat Completions API request body.
#[derive(Debug, Serialize, Deserialize)]
struct ChatCompletionsRequest {
    model: String,
    messages: Vec<ChatMessage>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    temperature: Option<f64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_tokens: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    stream_options: Option<Value>,
}

/// A single message in the Chat Completions format.
#[derive(Debug, Serialize, Deserialize, Clone)]
struct ChatMessage {
    role: String,
    content: Value,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_calls: Option<Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    tool_call_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    name: Option<String>,
}

// ---------------------------------------------------------------------------
// Content part type translation
// ---------------------------------------------------------------------------

/// Translate content part types from Responses API format to Chat Completions format.
///
/// The Responses API uses `input_text` and `input_image` as content part type
/// discriminators, while the Chat Completions API uses `text` and `image_url`.
/// Gateways that only speak Chat Completions will reject `input_text`/`input_image`
/// because they don't recognise those type strings.
///
/// This function handles:
/// - String content → pass through (Chat Completions accepts plain strings)
/// - Null content → pass through (e.g. assistant messages with tool_calls)
/// - Array content → translate each content part's type field
/// - Everything else → pass through as-is
fn translate_content(content: Value) -> Value {
    match content {
        // Null and string content are valid in Chat Completions — pass through.
        Value::Null | Value::String(_) => content,

        // Array of content parts — translate each part's type.
        Value::Array(parts) => {
            let translated: Vec<Value> = parts.into_iter().map(translate_content_part).collect();
            Value::Array(translated)
        }

        // Anything else (unusual but harmless) — pass through.
        other => other,
    }
}

/// Translate a single content part from Responses API to Chat Completions format.
///
/// Responses API content parts:
///   {"type": "input_text", "text": "..."}
///   {"type": "input_image", "image_url": {"url": "..."}}
///     or {"type": "input_image", "url": "..."}
///
/// Chat Completions content parts:
///   {"type": "text", "text": "..."}
///   {"type": "image_url", "image_url": {"url": "..."}}
fn translate_content_part(part: Value) -> Value {
    match &part {
        Value::Object(map) => {
            let part_type = map.get("type").and_then(|v| v.as_str()).unwrap_or("");

            match part_type {
                // input_text → text
                "input_text" => {
                    let text = map
                        .get("text")
                        .cloned()
                        .unwrap_or(Value::String(String::new()));
                    serde_json::json!({
                        "type": "text",
                        "text": text,
                    })
                }

                // input_image → image_url
                "input_image" => {
                    // Responses API can provide the URL nested as
                    //   {"image_url": {"url": "..."}}
                    // or flat as
                    //   {"url": "..."}
                    let url = map
                        .get("image_url")
                        .and_then(|v| v.get("url"))
                        .or_else(|| map.get("url"))
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    serde_json::json!({
                        "type": "image_url",
                        "image_url": {
                            "url": url,
                        },
                    })
                }

                // output_text → text (assistant message content parts)
                "output_text" => {
                    let text = map
                        .get("text")
                        .cloned()
                        .unwrap_or(Value::String(String::new()));
                    serde_json::json!({
                        "type": "text",
                        "text": text,
                    })
                }

                // Already in Chat Completions format or unknown type — pass through.
                _ => part,
            }
        }
        // Non-object content parts are unusual; pass through unchanged.
        _ => part,
    }
}

/// Merge consecutive assistant messages that each carry a single tool_call
/// into one assistant message with a combined tool_calls array.
///
/// The Responses API represents each function call as a separate input item,
/// so our translation produces N consecutive assistant messages each with one
/// tool_call. Chat Completions requires exactly one assistant message per turn
/// with all tool_calls grouped together. Without merging, strict endpoints
/// reject the request with a 422 error.
fn merge_consecutive_tool_call_messages(messages: &mut Vec<ChatMessage>) {
    let mut merged = Vec::with_capacity(messages.len());
    let mut i = 0;
    while i < messages.len() {
        let msg = messages[i].clone();
        // If this is an assistant message with tool_calls and null content,
        // keep scanning ahead for more consecutive assistant+tool_calls messages.
        if msg.role == "assistant" && msg.content.is_null() && msg.tool_calls.is_some() {
            let mut combined_calls: Vec<Value> = match &msg.tool_calls {
                Some(Value::Array(arr)) => arr.clone(),
                Some(v) => vec![v.clone()],
                None => vec![],
            };
            let mut j = i + 1;
            while j < messages.len()
                && messages[j].role == "assistant"
                && messages[j].content.is_null()
                && messages[j].tool_calls.is_some()
            {
                if let Some(Value::Array(arr)) = &messages[j].tool_calls {
                    combined_calls.extend(arr.iter().cloned());
                } else if let Some(v) = &messages[j].tool_calls {
                    combined_calls.push(v.clone());
                }
                j += 1;
            }
            merged.push(ChatMessage {
                role: "assistant".into(),
                content: Value::Null,
                tool_calls: Some(Value::Array(combined_calls)),
                tool_call_id: None,
                name: None,
            });
            i = j;
        } else {
            merged.push(msg);
            i += 1;
        }
    }
    *messages = merged;
}

// ---------------------------------------------------------------------------
// Request translation
// ---------------------------------------------------------------------------

/// Convert a Responses API request into a Chat Completions request.
fn translate_request(responses_req: &ResponsesApiRequest) -> ChatCompletionsRequest {
    let mut messages = Vec::new();

    // Handle instructions as system message
    if let Some(ref instructions) = responses_req.instructions {
        if !instructions.is_empty() {
            messages.push(ChatMessage {
                role: "system".into(),
                content: Value::String(instructions.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }

    // Handle input — can be a string or an array of content items
    match &responses_req.input {
        Value::String(s) => {
            messages.push(ChatMessage {
                role: "user".into(),
                content: Value::String(s.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
        Value::Array(arr) => {
            for item in arr {
                if let Value::Object(obj) = item {
                    let item_type = obj.get("type").and_then(|v| v.as_str()).unwrap_or("");
                    match item_type {
                        // Responses API function_call → Chat Completions assistant message with tool_calls
                        "function_call" => {
                            let call_id = obj.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                            let fn_name = obj.get("name").and_then(|v| v.as_str()).unwrap_or("");
                            let fn_args = obj
                                .get("arguments")
                                .and_then(|v| v.as_str())
                                .unwrap_or("{}");
                            messages.push(ChatMessage {
                                role: "assistant".into(),
                                content: Value::Null,
                                tool_calls: Some(serde_json::json!([{
                                    "id": call_id,
                                    "type": "function",
                                    "function": {
                                        "name": fn_name,
                                        "arguments": fn_args,
                                    }
                                }])),
                                tool_call_id: None,
                                name: None,
                            });
                        }
                        // Responses API function_call_output → Chat Completions tool message
                        "function_call_output" => {
                            let call_id = obj.get("call_id").and_then(|v| v.as_str()).unwrap_or("");
                            let output = obj.get("output").and_then(|v| v.as_str()).unwrap_or("");
                            messages.push(ChatMessage {
                                role: "tool".into(),
                                content: Value::String(output.to_string()),
                                tool_calls: None,
                                tool_call_id: Some(call_id.to_string()),
                                name: None,
                            });
                        }
                        // Regular message items with role/content — also carry
                        // over tool_call_id, tool_calls, and name which may be
                        // present on tool-response and assistant messages.
                        _ => {
                            let raw_role =
                                obj.get("role").and_then(|v| v.as_str()).unwrap_or("user");
                            // Map Responses API roles to Chat Completions roles.
                            // The Responses API uses "developer" for system-level
                            // instructions (equivalent to "system" in Chat Completions).
                            // Some OpenAI-compatible providers reject unknown roles,
                            // so we must translate "developer" → "system".
                            let role = match raw_role {
                                "developer" => "system",
                                other => other,
                            };
                            let content = translate_content(
                                obj.get("content").cloned().unwrap_or(Value::Null),
                            );

                            // Responses API tool-response items may appear as
                            // {"type":"message", "role":"tool", "tool_call_id":"…"}
                            // rather than {"type":"function_call_output", …}.
                            // Carry tool_call_id forward so the Chat Completions
                            // endpoint can validate tool-role messages correctly.
                            let tool_call_id = obj
                                .get("tool_call_id")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());

                            // Responses API assistant messages may include
                            // tool_calls directly (type "message" with role
                            // "assistant" and a tool_calls array).
                            let tool_calls = obj.get("tool_calls").cloned();

                            // The `name` field on a tool message identifies which
                            // function was called (optional in Chat Completions).
                            let name = obj
                                .get("name")
                                .and_then(|v| v.as_str())
                                .map(|s| s.to_string());

                            messages.push(ChatMessage {
                                role: role.to_string(),
                                content,
                                tool_calls,
                                tool_call_id,
                                name,
                            });
                        }
                    }
                } else {
                    // Fallback: treat array item as user content
                    messages.push(ChatMessage {
                        role: "user".into(),
                        content: translate_content(item.clone()),
                        tool_calls: None,
                        tool_call_id: None,
                        name: None,
                    });
                }
            }
        }
        other => {
            messages.push(ChatMessage {
                role: "user".into(),
                content: translate_content(other.clone()),
                tool_calls: None,
                tool_call_id: None,
                name: None,
            });
        }
    }

    // Merge consecutive assistant messages that have tool_calls.
    //
    // In the Responses API, each function_call item becomes a separate
    // input element, and our translation above creates one ChatMessage per
    // item. But Chat Completions requires all tool calls from a single
    // assistant turn to be in ONE message with a merged tool_calls array.
    // Without this, strict endpoints reject multiple consecutive assistant
    // messages each containing a single tool_call.
    merge_consecutive_tool_call_messages(&mut messages);

    // Translate tools — filter to "function" type AND reshape from Responses API
    // format (flat) to Chat Completions format (nested under "function" key).
    // Responses API: {type: "function", name: "...", description: "...", parameters: {...}}
    // Chat Completions: {type: "function", function: {name: "...", description: "...", parameters: {...}}}
    let tools = responses_req.tools.as_ref().and_then(|t| match t {
        Value::Array(tool_arr) => {
            let translated: Vec<Value> = tool_arr
                .iter()
                .filter(|tool| tool.get("type").and_then(|v| v.as_str()) == Some("function"))
                .map(|tool| {
                    // Reshape from flat Responses API format to nested Chat Completions format
                    let name = tool.get("name").cloned().unwrap_or(Value::Null);
                    let description = tool.get("description").cloned().unwrap_or(Value::Null);
                    let parameters = tool
                        .get("parameters")
                        .cloned()
                        .unwrap_or(Value::Object(serde_json::Map::new()));
                    serde_json::json!({
                        "type": "function",
                        "function": {
                            "name": name,
                            "description": description,
                            "parameters": parameters,
                        }
                    })
                })
                .collect();
            if translated.is_empty() {
                None
            } else {
                Some(Value::Array(translated))
            }
        }
        _ => None,
    });

    ChatCompletionsRequest {
        model: responses_req.model.clone(),
        messages,
        stream: responses_req.stream.or(Some(true)),
        temperature: responses_req.temperature,
        max_tokens: responses_req.max_output_tokens,
        tools,
        stream_options: if responses_req.stream.unwrap_or(true) {
            Some(serde_json::json!({"include_usage": true}))
        } else {
            None
        },
    }
}

// ---------------------------------------------------------------------------
// Response translation (non-streaming)
// ---------------------------------------------------------------------------

/// Translate a non-streaming Chat Completions response to a Responses API response.
fn translate_response(chat_response: Value, model: String) -> Value {
    let response_id = format!("resp_{}", Uuid::new_v4().to_string().replace("-", ""));
    let now = chrono::Utc::now().timestamp();

    let text = chat_response
        .pointer("/choices/0/message/content")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    let input_tokens = chat_response
        .pointer("/usage/prompt_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);
    let output_tokens = chat_response
        .pointer("/usage/completion_tokens")
        .and_then(|v| v.as_u64())
        .unwrap_or(0);

    // Build output items: function_call items first, then the text message.
    // In the Responses API, when the assistant makes tool calls, the
    // function_call items appear before the final text message in the
    // output array, matching the order in which the model produced them.
    let mut output_items = Vec::new();

    // Add function_call items first if present
    if let Some(Value::Array(calls)) = chat_response.pointer("/choices/0/message/tool_calls") {
        for call in calls {
            let func = &call["function"];
            output_items.push(serde_json::json!({
                "type": "function_call",
                "id": call.get("id").cloned().unwrap_or(serde_json::json!("call_0")),
                "call_id": call.get("id").cloned().unwrap_or(serde_json::json!("call_0")),
                "name": func.get("name").and_then(|v| v.as_str()).unwrap_or(""),
                "arguments": func.get("arguments").and_then(|v| v.as_str()).unwrap_or("{}"),
            }));
        }
    }

    // Then add the text message (even if empty, to match Responses API format)
    output_items.push(serde_json::json!({
        "type": "message",
        "id": format!("msg_{}", Uuid::new_v4().to_string().replace("-", "")),
        "status": "completed",
        "role": "assistant",
        "content": [{
            "type": "output_text",
            "text": text
        }]
    }));

    serde_json::json!({
        "id": response_id,
        "object": "response",
        "created_at": now,
        "model": model,
        "status": "completed",
        "output": output_items,
        "usage": {
            "input_tokens": input_tokens,
            "output_tokens": output_tokens,
            "total_tokens": input_tokens + output_tokens
        }
    })
}

// ---------------------------------------------------------------------------
// Proxy server
// ---------------------------------------------------------------------------

/// State for the proxy server.
#[derive(Clone)]
struct ProxyState {
    upstream_base_url: Arc<String>,
    api_key: Arc<String>,
    http_client: Arc<reqwest::Client>,
}

/// Start the Responses API proxy server.
///
/// Returns the local address the proxy is listening on (which should be used
/// as the `base_url` for the Codex custom provider config) and a shutdown
/// sender. When the sender is dropped or a shutdown signal is sent, the
/// server will drain in-flight requests before stopping.
pub async fn start_responses_proxy(
    upstream_base_url: String,
    api_key: String,
) -> Result<(SocketAddr, tokio::sync::watch::Sender<bool>)> {
    let http_client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(300))
        .build()
        .context("Failed to build HTTP client for responses proxy")?;

    let state = ProxyState {
        upstream_base_url: Arc::new(upstream_base_url),
        api_key: Arc::new(api_key),
        http_client: Arc::new(http_client),
    };

    // Bind to a random available port on localhost only
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .context("Failed to bind responses proxy listener")?;
    let addr = listener
        .local_addr()
        .context("Failed to get responses proxy local address")?;

    info!(
        "responses_proxy: listening on http://{}:{}",
        addr.ip(),
        addr.port()
    );

    // Register routes with both /v1/ prefix (OpenAI-compatible) and without
    // (Codex CLI v0.133.0+ sends to <base_url>/responses without /v1/).
    let app = axum::Router::new()
        .route("/v1/responses", axum::routing::post(handle_responses_post))
        .route("/v1/models", axum::routing::get(handle_models_get))
        .route("/responses", axum::routing::post(handle_responses_post))
        .route("/models", axum::routing::get(handle_models_get));

    let app = app.with_state(state);

    // Graceful shutdown: when the sender is dropped, the server will stop
    // accepting new connections and drain in-flight requests.
    let (shutdown_tx, mut shutdown_rx) = tokio::sync::watch::channel(false);

    tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app)
            .with_graceful_shutdown(async move {
                // Wait for shutdown signal
                let _ = shutdown_rx.changed().await;
                info!("responses_proxy: shutting down gracefully");
            })
            .await
        {
            error!("responses_proxy: server error: {}", e);
        }
    });

    Ok((addr, shutdown_tx))
}

use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};

/// Handle POST /v1/responses — translate to Chat Completions and proxy.
async fn handle_responses_post(
    state: State<ProxyState>,
    headers: HeaderMap,
    body: axum::body::Bytes,
) -> Response {
    // Parse the incoming Responses API request
    let req: ResponsesApiRequest = match serde_json::from_slice(&body) {
        Ok(r) => r,
        Err(e) => {
            error!("responses_proxy: failed to parse request body: {}", e);
            return (
                StatusCode::BAD_REQUEST,
                format!("Invalid request body: {}", e),
            )
                .into_response();
        }
    };

    let model = req.model.clone();
    let is_streaming = req.stream.unwrap_or(true);

    // Warn if previous_response_id is present — we don't support conversation continuity
    if req.extra.get("previous_response_id").is_some() {
        warn!(
            "responses_proxy: request includes previous_response_id which is NOT supported — \
             conversation history will be incomplete. This may cause 422 errors if the \
             request depends on prior tool call context."
        );
    }

    debug!(
        "responses_proxy: translating /v1/responses → /v1/chat/completions (model={}, stream={})",
        model, is_streaming
    );

    // Translate to Chat Completions format
    let chat_req = translate_request(&req);
    let chat_body = match serde_json::to_vec(&chat_req) {
        Ok(b) => b,
        Err(e) => {
            error!(
                "responses_proxy: failed to serialize Chat Completions request: {}",
                e
            );
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("Serialization error: {}", e),
            )
                .into_response();
        }
    };

    // Forward to upstream /v1/chat/completions
    let upstream_url = format!(
        "{}/chat/completions",
        state.upstream_base_url.trim_end_matches('/')
    );

    let mut upstream_req = state
        .http_client
        .post(&upstream_url)
        .header("Content-Type", "application/json")
        .header("Authorization", format!("Bearer {}", state.api_key));

    // Forward relevant headers (but NOT Authorization — we add our own)
    if let Some(accept) = headers.get("accept") {
        upstream_req = upstream_req.header("Accept", accept);
    }

    if is_streaming {
        upstream_req = upstream_req.header("Accept", "text/event-stream");
    }

    let upstream_resp = match upstream_req
        .body(reqwest::Body::from(chat_body))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            error!("responses_proxy: upstream request failed: {}", e);
            return (
                StatusCode::BAD_GATEWAY,
                format!("Upstream request failed: {}", e),
            )
                .into_response();
        }
    };

    let upstream_status = upstream_resp.status();

    if !upstream_status.is_success() {
        let body = upstream_resp.text().await.unwrap_or_default();
        error!(
            "responses_proxy: upstream returned {}: {}",
            upstream_status, body
        );
        // Diagnostic: on 422 errors, log the translated messages for debugging
        if upstream_status.as_u16() == 422 {
            let msg_summary: Vec<String> = chat_req
                .messages
                .iter()
                .enumerate()
                .map(|(i, m)| {
                    let has_tool_calls = m.tool_calls.is_some();
                    let has_tool_call_id = m.tool_call_id.is_some();
                    let content_preview = match &m.content {
                        Value::String(s) => format!("{} chars", s.len()),
                        Value::Array(arr) => format!("{} items", arr.len()),
                        Value::Null => "null".to_string(),
                        other => format!("{:?}", other),
                    };
                    format!(
                        "[{}] role={} content={} tool_calls={} tool_call_id={}",
                        i, m.role, content_preview, has_tool_calls, has_tool_call_id
                    )
                })
                .collect();
            debug!(
                "responses_proxy: 422 — translated messages that caused the error: [{}]",
                msg_summary.join(" | ")
            );
        }
        return (upstream_status, body).into_response();
    }

    // Check if the response is streaming
    let content_type = upstream_resp
        .headers()
        .get("content-type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");

    if content_type.contains("text/event-stream") {
        // Streaming response — translate SSE events from Chat Completions format
        // to Responses API format in real time, forwarding tokens as they arrive.
        //
        // The Responses API streaming protocol requires a specific sequence of events:
        // 1. response.created
        // 2. response.output_item.added  (for each output item: message or function_call)
        // 3. response.content_part.added (for message items, before text deltas)
        // 4. response.output_text.delta (streaming text content)
        // 5. response.output_text.done    (text content complete)
        // 6. response.content_part.done   (content part complete)
        // 7. response.output_item.done    (output item complete)
        // 8. response.function_call_arguments.delta (for function calls)
        // 9. response.function_call_arguments.done   (function call complete)
        // 10. response.completed
        //
        // Codex CLI requires ALL of these to properly track its internal state
        // machine. Missing events cause "OutputTextDelta without active item"
        // errors which silently discard the model's response.
        let response_id = format!("resp_{}", Uuid::new_v4().to_string().replace("-", ""));
        let model_for_stream = model.clone();
        let msg_id = format!("msg_{}", Uuid::new_v4().to_string().replace("-", ""));
        let _api_key = state.api_key.clone();
        let _upstream_base = state.upstream_base_url.clone();

        // Create a channel to stream translated SSE events
        let (tx, rx) = tokio::sync::mpsc::channel::<Result<String, std::io::Error>>(256);

        // Emit the initial response.created event
        let created_event = serde_json::json!({
            "type": "response.created",
            "response": {
                "id": response_id,
                "object": "response",
                "created_at": chrono::Utc::now().timestamp(),
                "status": "in_progress",
                "model": model,
                "output": []
            }
        });

        if tx
            .try_send(Ok(format!(
                "event: response.created\ndata: {}\n\n",
                created_event
            )))
            .is_err()
        {
            // Receiver already dropped — client disconnected
            return StatusCode::OK.into_response();
        }

        // Spawn a task to read the upstream SSE stream and translate events in real time
        tokio::spawn(async move {
            use futures::StreamExt;

            let mut buffer = String::new();
            let mut input_tokens: u64 = 0;
            let mut output_tokens: u64 = 0;
            let mut full_text = String::new();
            // Track whether we've emitted the output_item.added and
            // content_part.added events for the text message.
            let mut text_item_emitted = false;
            // Track the output index for items. Starts at 0 for the first item.
            let mut output_index: usize = 0;

            // Track function calls across streaming chunks.
            let mut pending_fn_calls: std::collections::HashMap<String, PendingFnCall> =
                std::collections::HashMap::new();

            let mut stream = upstream_resp.bytes_stream();

            while let Some(chunk_result) = stream.next().await {
                match chunk_result {
                    Ok(chunk) => {
                        buffer.push_str(&String::from_utf8_lossy(&chunk));

                        // Process complete lines
                        while let Some(pos) = buffer.find('\n') {
                            let line = buffer[..pos].to_string();
                            // Use drain to modify in-place instead of allocating a
                            // new String for the remainder on every line.
                            buffer.drain(..pos + 1);

                            let line = line.trim();
                            if line.is_empty() || line.starts_with(':') {
                                continue;
                            }

                            if let Some(data) = line.strip_prefix("data: ") {
                                if data == "[DONE]" {
                                    // End of stream — finalize the text message item
                                    // and any pending function calls, then emit
                                    // response.completed.

                                    // Finalize the text message if we emitted it.
                                    if text_item_emitted {
                                        // Emit output_text.done
                                        let text_done = serde_json::json!({
                                            "type": "response.output_text.done",
                                            "output_index": 0,
                                            "content_index": 0,
                                            "text": full_text
                                        });
                                        if tx
                                            .try_send(Ok(format!(
                                                "event: response.output_text.done\ndata: {}\n\n",
                                                text_done
                                            )))
                                            .is_err()
                                        {
                                            return;
                                        }

                                        // Emit content_part.done
                                        let content_done = serde_json::json!({
                                            "type": "response.content_part.done",
                                            "output_index": 0,
                                            "content_index": 0,
                                            "part": {
                                                "type": "output_text",
                                                "text": full_text
                                            }
                                        });
                                        if tx
                                            .try_send(Ok(format!(
                                                "event: response.content_part.done\ndata: {}\n\n",
                                                content_done
                                            )))
                                            .is_err()
                                        {
                                            return;
                                        }

                                        // Emit output_item.done for the message
                                        let msg_done = serde_json::json!({
                                            "type": "response.output_item.done",
                                            "output_index": 0,
                                            "item": {
                                                "type": "message",
                                                "id": msg_id,
                                                "role": "assistant",
                                                "content": [{
                                                    "type": "output_text",
                                                    "text": full_text
                                                }],
                                                "status": "completed"
                                            }
                                        });
                                        if tx
                                            .try_send(Ok(format!(
                                                "event: response.output_item.done\ndata: {}\n\n",
                                                msg_done
                                            )))
                                            .is_err()
                                        {
                                            return;
                                        }
                                    }

                                    // Emit function_call_arguments.done and
                                    // output_item.done for each pending function call.
                                    for (call_id, fc) in &pending_fn_calls {
                                        let fn_done = serde_json::json!({
                                            "type": "response.function_call_arguments.done",
                                            "output_index": output_index,
                                            "call_id": call_id,
                                            "name": fc.name,
                                            "arguments": fc.arguments
                                        });
                                        if tx.try_send(Ok(format!(
                                            "event: response.function_call_arguments.done\ndata: {}\n\n",
                                            fn_done
                                        ))).is_err() {
                                            return;
                                        }

                                        let item_done = serde_json::json!({
                                            "type": "response.output_item.done",
                                            "output_index": output_index,
                                            "item": {
                                                "type": "function_call",
                                                "id": call_id,
                                                "call_id": call_id,
                                                "name": fc.name,
                                                "arguments": fc.arguments,
                                                "status": "completed"
                                            }
                                        });
                                        if tx
                                            .try_send(Ok(format!(
                                                "event: response.output_item.done\ndata: {}\n\n",
                                                item_done
                                            )))
                                            .is_err()
                                        {
                                            return;
                                        }
                                        output_index += 1;
                                    }

                                    // Build output items for the completed response.
                                    let mut output_items = Vec::new();

                                    // Function call items come first (matching the order
                                    // they appeared in the stream).
                                    for (call_id, fc) in &pending_fn_calls {
                                        output_items.push(serde_json::json!({
                                            "type": "function_call",
                                            "id": call_id,
                                            "call_id": call_id,
                                            "name": fc.name,
                                            "arguments": fc.arguments,
                                            "status": "completed"
                                        }));
                                    }

                                    // Then the text message (even if empty).
                                    output_items.push(serde_json::json!({
                                        "type": "message",
                                        "id": msg_id,
                                        "role": "assistant",
                                        "content": [{
                                            "type": "output_text",
                                            "text": full_text
                                        }],
                                        "status": "completed"
                                    }));

                                    let completed_event = serde_json::json!({
                                        "type": "response.completed",
                                        "response": {
                                            "id": response_id,
                                            "status": "completed",
                                            "model": model_for_stream,
                                            "output": output_items,
                                            "usage": {
                                                "input_tokens": input_tokens,
                                                "output_tokens": output_tokens,
                                                "total_tokens": input_tokens + output_tokens
                                            }
                                        }
                                    });
                                    let _ = tx.try_send(Ok(format!(
                                        "event: response.completed\ndata: {}\n\n",
                                        completed_event
                                    )));
                                    let _ = tx.try_send(Ok("data: [DONE]\n\n".to_string()));
                                    return;
                                }

                                if let Ok(chunk) = serde_json::from_str::<Value>(data) {
                                    // Extract delta text content
                                    if let Some(text) = chunk.pointer("/choices/0/delta/content") {
                                        if let Some(s) = text.as_str() {
                                            if !s.is_empty() {
                                                // On first text content, emit the
                                                // output_item.added and content_part.added
                                                // events so Codex CLI has an active item
                                                // to put text deltas into.
                                                if !text_item_emitted {
                                                    text_item_emitted = true;

                                                    // Emit response.output_item.added for the message
                                                    let item_added = serde_json::json!({
                                                        "type": "response.output_item.added",
                                                        "output_index": 0,
                                                        "item": {
                                                            "type": "message",
                                                            "id": msg_id,
                                                            "role": "assistant",
                                                            "content": [],
                                                            "status": "in_progress"
                                                        }
                                                    });
                                                    if tx.try_send(Ok(format!(
                                                        "event: response.output_item.added\ndata: {}\n\n",
                                                        item_added
                                                    ))).is_err() {
                                                        return; // Receiver dropped
                                                    }

                                                    // Emit response.content_part.added for the text part
                                                    let content_added = serde_json::json!({
                                                        "type": "response.content_part.added",
                                                        "output_index": 0,
                                                        "content_index": 0,
                                                        "part": {
                                                            "type": "output_text",
                                                            "text": ""
                                                        }
                                                    });
                                                    if tx.try_send(Ok(format!(
                                                        "event: response.content_part.added\ndata: {}\n\n",
                                                        content_added
                                                    ))).is_err() {
                                                        return; // Receiver dropped
                                                    }
                                                }

                                                full_text.push_str(s);
                                                let delta_event = serde_json::json!({
                                                    "type": "response.output_text.delta",
                                                    "output_index": 0,
                                                    "content_index": 0,
                                                    "delta": s
                                                });
                                                if tx.try_send(Ok(format!(
                                                    "event: response.output_text.delta\ndata: {}\n\n",
                                                    delta_event
                                                ))).is_err() {
                                                    return; // Receiver dropped
                                                }
                                            }
                                        }
                                    }

                                    // Extract tool call deltas
                                    // In Chat Completions streaming, the first chunk for each
                                    // tool call carries id + name + first arguments chunk.
                                    // Subsequent chunks carry only incremental arguments
                                    // (id and name are absent/empty).
                                    if let Some(Value::Array(calls)) =
                                        chunk.pointer("/choices/0/delta/tool_calls")
                                    {
                                        for call in calls {
                                            let call_id = call
                                                .get("id")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            let fn_name = call
                                                .pointer("/function/name")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");
                                            let fn_args = call
                                                .pointer("/function/arguments")
                                                .and_then(|v| v.as_str())
                                                .unwrap_or("");

                                            // When we first see a call_id, it's the start
                                            // of a new function call. Emit
                                            // response.output_item.added so the client
                                            // knows a function call is starting.
                                            if !call_id.is_empty() {
                                                let is_new =
                                                    !pending_fn_calls.contains_key(call_id);
                                                if is_new {
                                                    // Emit response.output_item.added event
                                                    // for the new function call.
                                                    let added_event = serde_json::json!({
                                                        "type": "response.output_item.added",
                                                        "output_index": output_index,
                                                        "item": {
                                                            "type": "function_call",
                                                            "id": call_id,
                                                            "call_id": call_id,
                                                            "name": fn_name,
                                                            "arguments": "",
                                                            "status": "in_progress"
                                                        }
                                                    });
                                                    if tx.try_send(Ok(format!(
                                                        "event: response.output_item.added\ndata: {}\n\n",
                                                        added_event
                                                    ))).is_err() {
                                                        return;
                                                    }
                                                    pending_fn_calls.insert(
                                                        call_id.to_string(),
                                                        PendingFnCall {
                                                            name: fn_name.to_string(),
                                                            arguments: String::new(),
                                                        },
                                                    );
                                                    output_index += 1;
                                                }
                                            }

                                            // Accumulate arguments for the function call.
                                            if !fn_args.is_empty() {
                                                let target_id = if !call_id.is_empty() {
                                                    Some(call_id.to_string())
                                                } else {
                                                    // Later chunks may omit call_id;
                                                    // find the most recent pending call.
                                                    pending_fn_calls.keys().last().cloned()
                                                };

                                                if let Some(target_id) = target_id {
                                                    if let Some(fc) =
                                                        pending_fn_calls.get_mut(&target_id)
                                                    {
                                                        fc.arguments.push_str(fn_args);
                                                    }
                                                }
                                            }

                                            // Emit argument delta events for the client.
                                            if !fn_args.is_empty() || !fn_name.is_empty() {
                                                let delta_event = serde_json::json!({
                                                    "type": "response.function_call_arguments.delta",
                                                    "call_id": call_id,
                                                    "name": fn_name,
                                                    "delta": fn_args
                                                });
                                                if tx.try_send(Ok(format!(
                                                    "event: response.function_call_arguments.delta\ndata: {}\n\n",
                                                    delta_event
                                                ))).is_err() {
                                                    return;
                                                }
                                            }
                                        }
                                    }

                                    // Extract usage from final chunk
                                    if let Some(usage) = chunk.get("usage") {
                                        input_tokens = usage
                                            .get("prompt_tokens")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(input_tokens);
                                        output_tokens = usage
                                            .get("completion_tokens")
                                            .and_then(|v| v.as_u64())
                                            .unwrap_or(output_tokens);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => {
                        error!("responses_proxy: stream error: {}", e);
                        break;
                    }
                }
            }

            // If we reach here without [DONE], the upstream stream disconnected.
            // Emit completion events for whatever partial content we received so
            // Codex can salvage the response rather than crashing with
            // "stream disconnected before completion: missing field `total_tokens`".
            if text_item_emitted {
                // Finalize the text message
                let text_done = serde_json::json!({
                    "type": "response.output_text.done",
                    "output_index": 0,
                    "content_index": 0,
                    "text": full_text
                });
                let _ = tx.try_send(Ok(format!(
                    "event: response.output_text.done\ndata: {}\n\n",
                    text_done
                )));

                let content_done = serde_json::json!({
                    "type": "response.content_part.done",
                    "output_index": 0,
                    "content_index": 0,
                    "part": {
                        "type": "output_text",
                        "text": full_text
                    }
                });
                let _ = tx.try_send(Ok(format!(
                    "event: response.content_part.done\ndata: {}\n\n",
                    content_done
                )));

                let msg_done = serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": 0,
                    "item": {
                        "type": "message",
                        "id": msg_id,
                        "role": "assistant",
                        "content": [{
                            "type": "output_text",
                            "text": full_text
                        }],
                        "status": "completed"
                    }
                });
                let _ = tx.try_send(Ok(format!(
                    "event: response.output_item.done\ndata: {}\n\n",
                    msg_done
                )));
            }

            // Finalize any pending function calls
            for (call_id, fc) in &pending_fn_calls {
                let fn_done = serde_json::json!({
                    "type": "response.function_call_arguments.done",
                    "output_index": output_index,
                    "call_id": call_id,
                    "name": fc.name,
                    "arguments": fc.arguments
                });
                let _ = tx.try_send(Ok(format!(
                    "event: response.function_call_arguments.done\ndata: {}\n\n",
                    fn_done
                )));

                let item_done = serde_json::json!({
                    "type": "response.output_item.done",
                    "output_index": output_index,
                    "item": {
                        "type": "function_call",
                        "id": call_id,
                        "call_id": call_id,
                        "name": fc.name,
                        "arguments": fc.arguments,
                        "status": "completed"
                    }
                });
                let _ = tx.try_send(Ok(format!(
                    "event: response.output_item.done\ndata: {}\n\n",
                    item_done
                )));
                output_index += 1;
            }

            // Build output items for the completed response
            let mut output_items = Vec::new();
            for (call_id, fc) in &pending_fn_calls {
                output_items.push(serde_json::json!({
                    "type": "function_call",
                    "id": call_id,
                    "call_id": call_id,
                    "name": fc.name,
                    "arguments": fc.arguments,
                    "status": "completed"
                }));
            }
            output_items.push(serde_json::json!({
                "type": "message",
                "id": msg_id,
                "role": "assistant",
                "content": [{
                    "type": "output_text",
                    "text": full_text
                }],
                "status": "completed"
            }));

            let completed_event = serde_json::json!({
                "type": "response.completed",
                "response": {
                    "id": response_id,
                    "status": "completed",
                    "model": model_for_stream,
                    "output": output_items,
                    "usage": {
                        "input_tokens": input_tokens,
                        "output_tokens": output_tokens,
                        "total_tokens": input_tokens + output_tokens
                    }
                }
            });
            let _ = tx.try_send(Ok(format!(
                "event: response.completed\ndata: {}\n\n",
                completed_event
            )));
            let _ = tx.try_send(Ok("data: [DONE]\n\n".to_string()));
        });

        // Convert the channel into a streaming response
        let body_stream = tokio_stream::wrappers::ReceiverStream::new(rx);
        let body = Body::from_stream(body_stream);

        (
            StatusCode::OK,
            [
                ("content-type", "text/event-stream"),
                ("cache-control", "no-cache"),
                ("connection", "keep-alive"),
            ],
            body,
        )
            .into_response()
    } else {
        // Non-streaming response — translate the JSON response
        let body = match upstream_resp.bytes().await {
            Ok(b) => b,
            Err(e) => {
                error!("responses_proxy: failed to read upstream response: {}", e);
                return (
                    StatusCode::BAD_GATEWAY,
                    format!("Failed to read upstream response: {}", e),
                )
                    .into_response();
            }
        };

        let chat_response: Value = match serde_json::from_slice(&body) {
            Ok(v) => v,
            Err(e) => {
                error!("responses_proxy: failed to parse upstream JSON: {}", e);
                return (StatusCode::BAD_GATEWAY, body).into_response();
            }
        };

        let response = translate_response(chat_response, model);
        let response_body = match serde_json::to_string_pretty(&response) {
            Ok(s) => s,
            Err(e) => {
                error!("responses_proxy: failed to serialize response: {}", e);
                return (
                    StatusCode::INTERNAL_SERVER_ERROR,
                    "Serialization error".to_string(),
                )
                    .into_response();
            }
        };

        (
            StatusCode::OK,
            [("content-type", "application/json")],
            response_body,
        )
            .into_response()
    }
}

/// Handle GET /v1/models — pass through to upstream.
async fn handle_models_get(state: State<ProxyState>, _headers: HeaderMap) -> Response {
    let upstream_url = format!("{}/models", state.upstream_base_url.trim_end_matches('/'));

    match state
        .http_client
        .get(&upstream_url)
        .header("Authorization", format!("Bearer {}", state.api_key))
        .send()
        .await
    {
        Ok(resp) => {
            let status = resp.status();
            match resp.bytes().await {
                Ok(body) => {
                    let mut response = (status, body.to_vec()).into_response();
                    response
                        .headers_mut()
                        .insert("content-type", HeaderValue::from_static("application/json"));
                    response
                }
                Err(e) => {
                    error!("responses_proxy: failed to read upstream models: {}", e);
                    (StatusCode::BAD_GATEWAY, format!("Upstream error: {}", e)).into_response()
                }
            }
        }
        Err(e) => {
            error!("responses_proxy: upstream models request failed: {}", e);
            (
                StatusCode::BAD_GATEWAY,
                format!("Upstream unavailable: {}", e),
            )
                .into_response()
        }
    }
}
