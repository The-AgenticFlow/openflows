use anyhow::Result;
use std::process::Command;

use super::SetupConfig;

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub slug: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

/// Discover available models from the selected provider.
/// Fetches models dynamically from Anthropic, OpenAI, or Fireworks API using user's key.
pub async fn discover_models(config: &SetupConfig) -> Result<Vec<ModelInfo>> {
    match config.selected_provider.as_deref() {
        Some(p) if p.contains("Anthropic") => {
            // Try Anthropic API first with user's key
            if !config.anthropic_key.is_empty() {
                match discover_anthropic_models(&config.anthropic_key).await {
                    Ok(models) if !models.is_empty() => return Ok(models),
                    Ok(_) => {
                        tracing::warn!("Anthropic API returned no models, falling back");
                    }
                    Err(e) => {
                        tracing::warn!(error = %e, "Anthropic API model discovery failed, falling back");
                    }
                }
            }
            // Try CLI discovery as second fallback
            if let Ok(models) = discover_claude_models_from_cli() {
                if !models.is_empty() {
                    return Ok(models);
                }
            }
            // Minimal hardcoded fallback — only used when API and CLI are both unreachable
            tracing::warn!(
                "All Anthropic model discovery methods failed — using minimal fallback list. \
                 Models may not reflect your account's available models."
            );
            discover_known_anthropic_models()
        }
        Some(p) if p.contains("OpenAI") || p.contains("Codex") => {
            // For OpenAI/Codex, try Codex CLI first, then OpenAI API
            if let Ok(models) = discover_codex_models() {
                if !models.is_empty() {
                    return Ok(models);
                }
            }
            // Fallback to OpenAI API
            if let Some(ref key) = config.openai_key {
                if !key.is_empty() {
                    match discover_openai_models(key).await {
                        Ok(models) if !models.is_empty() => return Ok(models),
                        Ok(_) => {
                            tracing::warn!("OpenAI API returned no models, falling back");
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "OpenAI API model discovery failed, falling back");
                        }
                    }
                }
            }
            // Minimal hardcoded fallback
            tracing::warn!(
                "All OpenAI model discovery methods failed — using minimal fallback list."
            );
            discover_known_openai_models()
        }
        Some(p) if p.contains("Fireworks") => {
            if let Some(ref key) = config.fireworks_key {
                if !key.is_empty() {
                    match discover_fireworks_models(key).await {
                        Ok(models) if !models.is_empty() => {
                            tracing::info!(
                                count = models.len(),
                                "Discovered Fireworks models via live API"
                            );
                            return Ok(models);
                        }
                        Ok(_) => {
                            tracing::warn!(
                                "Fireworks API returned 0 chat models — falling back to minimal list"
                            );
                        }
                        Err(e) => {
                            tracing::warn!(
                                error = %e,
                                "Fireworks API model discovery failed — falling back to minimal list"
                            );
                        }
                    }
                } else {
                    tracing::warn!(
                        "Fireworks API key is empty — skipping live discovery, using minimal list"
                    );
                }
            } else {
                tracing::warn!(
                    "No Fireworks API key configured — skipping live discovery, using minimal list"
                );
            }
            discover_known_fireworks_models()
        }
        _ => discover_backend_models(&config.selected_cli_backend),
    }
}

fn discover_backend_models(cli_backend: &str) -> Result<Vec<ModelInfo>> {
    match cli_backend {
        "codex" => discover_codex_models(),
        "claude" => discover_claude_models_from_cli(),
        other => Err(anyhow::anyhow!("Unknown CLI backend: {}", other)),
    }
}

/// Query a custom (OpenAI-compatible) provider for available models.
#[allow(dead_code)]
async fn discover_custom_provider_models(
    url: &str,
    api_key: Option<&str>,
) -> Result<Vec<ModelInfo>> {
    let client = reqwest::Client::new();
    let models_url = format!("{}/models", url.trim_end_matches('/'));

    let mut request = client.get(&models_url);
    if let Some(key) = api_key {
        request = request.header("Authorization", format!("Bearer {}", key));
    }

    let response = request.send().await?;
    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Custom provider returned status {} when listing models",
            response.status()
        ));
    }

    let json: serde_json::Value = response.json().await?;
    let mut models = Vec::new();

    if let Some(model_array) = json.get("data").and_then(|v| v.as_array()) {
        for m in model_array {
            let raw_id = m
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let display_name = m
                .get("name")
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let description = m
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);

            models.push(ModelInfo {
                slug: raw_id,
                display_name,
                description,
            });
        }
    }

    if models.is_empty() {
        return Err(anyhow::anyhow!("No models found at custom provider"));
    }

    Ok(models)
}

/// Fetch models from codex debug models command.
/// Discovered slugs are normalised with an `openai/` prefix so the
/// fallback client chain can route them correctly without relying on
/// the MODEL_PROVIDER_MAP environment variable.
fn discover_codex_models() -> Result<Vec<ModelInfo>> {
    let output = Command::new("codex").args(["debug", "models"]).output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr);
        return Err(anyhow::anyhow!(
            "codex debug models failed: {}",
            stderr.lines().next().unwrap_or("")
        ));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let json: serde_json::Value = serde_json::from_str(&stdout)?;

    let mut models = Vec::new();
    if let Some(model_array) = json.get("models").and_then(|v| v.as_array()) {
        for m in model_array {
            let raw_slug = m
                .get("slug")
                .or_else(|| m.get("name"))
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            // Normalise: prefix bare model IDs with openai/ so the
            // fallback chain knows which provider to use.
            let slug = if raw_slug.contains('/') {
                raw_slug
            } else {
                format!("openai/{}", raw_slug)
            };

            let display_name = m
                .get("display_name")
                .or_else(|| m.get("name"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let description = m
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);

            models.push(ModelInfo {
                slug,
                display_name,
                description,
            });
        }
    }

    Ok(models)
}

/// Fetch available models from Anthropic API using user's API key.
/// Uses pagination to retrieve all available models.
async fn discover_anthropic_models(api_key: &str) -> Result<Vec<ModelInfo>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut all_models: Vec<ModelInfo> = Vec::new();
    let mut after_id: Option<String> = None;
    let max_pages = 10;
    let mut page_count = 0;

    loop {
        if page_count >= max_pages {
            break;
        }
        let mut url = "https://api.anthropic.com/v1/models?limit=1000".to_string();
        if let Some(ref id) = after_id {
            url.push_str(&format!("&after_id={}", id));
        }

        let response = client
            .get(&url)
            .header("x-api-key", api_key)
            .header("anthropic-version", "2023-06-01")
            .send()
            .await?;

        if !response.status().is_success() {
            let status = response.status();
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "Anthropic models API error");
            return Err(anyhow::anyhow!(
                "Anthropic API returned status {} when listing models",
                status
            ));
        }

        let json: serde_json::Value = response.json().await?;
        let has_more = json
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        let model_array = json
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        for m in &model_array {
            let raw_id = m
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            // Exclude non-chat models
            if !is_valid_anthropic_api_model(&raw_id) {
                continue;
            }

            let display_name = m
                .get("display_name")
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let description = m
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);

            all_models.push(ModelInfo {
                slug: format!("anthropic/{}", raw_id),
                display_name,
                description,
            });
        }

        if !has_more || model_array.is_empty() {
            break;
        }

        page_count += 1;

        // Use the last model ID as the pagination cursor
        if let Some(last) = model_array.last() {
            let next = last.get("id").and_then(|v| v.as_str()).map(String::from);
            if next.as_ref().map_or(true, |t| t.is_empty()) {
                break;
            }
            after_id = next;
        } else {
            break;
        }
    }

    tracing::info!(count = all_models.len(), "Discovered Anthropic models via API");

    if all_models.is_empty() {
        return Err(anyhow::anyhow!("No models found in Anthropic account"));
    }

    Ok(all_models)
}

/// Validates an Anthropic model ID returned from /v1/models.
/// Accepts any `claude-*` ID (dated versions like claude-3-5-sonnet-20241022
/// and newer versioned IDs like claude-opus-4-6).
/// Rejects `-latest` aliases and RLHF/eval variants.
fn is_valid_anthropic_api_model(model_id: &str) -> bool {
    // Anthropic model IDs from /v1/models include both dated versions and
    // newer versioned IDs such as claude-opus-4-6. We accept any claude-* ID
    // that is not a -latest alias and is not an RLHF/eval variant.
    if !model_id.starts_with("claude-") {
        return false;
    }
    if model_id.ends_with("-latest") {
        return false;
    }
    let parts: Vec<&str> = model_id.split('-').collect();
    if parts.len() < 3 {
        return false;
    }
    if model_id.contains("-rlhf-") || model_id.contains("-eval-") {
        return false;
    }
    true
}

/// Check if an Anthropic model ID is a chat model
#[allow(dead_code)]
fn is_anthropic_chat_model(model_id: &str) -> bool {
    // Only accept models that pass the valid API model check
    model_id.starts_with("claude-")
        && !model_id.contains("-rlhf-")  // Exclude RLHF training models
        && !model_id.contains("-eval-") // Exclude evaluation models
        && is_valid_anthropic_api_model(model_id) // Must be a valid API model ID
}

/// Fetch models from claude CLI — called directly as a fallback step.
/// No longer has its own hardcoded fallback; that is handled by
/// discover_known_anthropic_models() at the orchestration level.
fn discover_claude_models_from_cli() -> Result<Vec<ModelInfo>> {
    let output = Command::new("claude").args(["models"]).output()?;

    if !output.status.success() {
        return Err(anyhow::anyhow!("claude models command failed"));
    }

    let stdout = String::from_utf8_lossy(&output.stdout);
    let mut models = Vec::new();

    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Map CLI -latest aliases to actual API model IDs
        let api_model_id = map_claude_model_alias(line);

        // Only include models that are valid Anthropic API model IDs
        // This filters out marketing names like claude-sonnet-4-6
        if !is_valid_anthropic_api_model(api_model_id) {
            tracing::debug!(
                "Skipping invalid Anthropic model ID from CLI: {}",
                api_model_id
            );
            continue;
        }

        models.push(ModelInfo {
            slug: format!("anthropic/{}", api_model_id),
            display_name: Some(line.replace('-', " ").to_string()),
            description: None,
        });
    }

    if models.is_empty() {
        return Err(anyhow::anyhow!("no models found in claude CLI output"));
    }

    Ok(models)
}

/// Map Claude CLI model aliases to actual API model IDs
/// The -latest aliases don't work with the API
fn map_claude_model_alias(alias: &str) -> &str {
    match alias {
        "claude-3-5-sonnet-latest" => "claude-3-5-sonnet-20241022",
        "claude-3-5-haiku-latest" => "claude-3-5-haiku-20241022",
        "claude-3-opus-latest" => "claude-3-opus-20240229",
        "claude-3-haiku-latest" => "claude-3-haiku-20240307",
        // If it's already a dated version or unknown, return as-is
        _ => alias,
    }
}

/// Fetch available models from OpenAI API.
/// Uses pagination to retrieve all available models.
async fn discover_openai_models(api_key: &str) -> Result<Vec<ModelInfo>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut all_models: Vec<ModelInfo> = Vec::new();

    let url = "https://api.openai.com/v1/models?limit=500";

    let response = match client
        .get(url)
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await
    {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "OpenAI models request failed");
            return Err(anyhow::anyhow!(
                "Failed to reach OpenAI API: {}. Check your network connection.",
                e
            ));
        }
    };

    let status = response.status();
    if !status.is_success() {
        let body = response.text().await.unwrap_or_default();
        tracing::warn!(status = %status, body = %body, "OpenAI models API error");
        return Err(anyhow::anyhow!(
            "OpenAI API returned status {} when listing models: {}",
            status,
            body.chars().take(200).collect::<String>()
        ));
    }

    let json: serde_json::Value = match response.json().await {
        Ok(v) => v,
        Err(e) => {
            tracing::warn!(error = %e, "Failed to parse OpenAI models response as JSON");
            return Err(anyhow::anyhow!(
                "Failed to parse OpenAI models response: {}",
                e
            ));
        }
    };

    let model_array = json
        .get("data")
        .and_then(|v| v.as_array())
        .cloned()
        .unwrap_or_default();

    for m in &model_array {
        let raw_id = m
            .get("id")
            .and_then(|v| v.as_str())
            .unwrap_or("unknown")
            .to_string();

        // Skip non-chat models and embeddings
        if !is_openai_chat_model(&raw_id) {
            continue;
        }

        let display_name = m
            .get("name")
            .or_else(|| m.get("id"))
            .and_then(|v| v.as_str())
            .map(String::from);

        all_models.push(ModelInfo {
            slug: format!("openai/{}", raw_id),
            display_name,
            description: None,
        });
    }

    tracing::info!(count = all_models.len(), "Discovered OpenAI models via API");

    if all_models.is_empty() {
        return Err(anyhow::anyhow!(
            "No chat models found in OpenAI account"
        ));
    }

    Ok(all_models)
}

/// Check if an OpenAI model ID is a chat model (not embeddings, audio, etc.)
fn is_openai_chat_model(model_id: &str) -> bool {
    // Include GPT models and exclude non-chat models
    (model_id.starts_with("gpt-") || model_id.starts_with("o1") || model_id.starts_with("o3"))
        && !model_id.contains("embedding")
        && !model_id.contains("whisper")
        && !model_id.contains("tts")
        && !model_id.contains("dall-e")
        && !model_id.contains("audio")
        && !model_id.contains("realtime")
}

/// Fetch available models from Fireworks API using the native
/// `/v1/accounts/fireworks/models` endpoint (the OpenAI-compatible
/// `/inference/v1/models` endpoint has been returning 500 errors).
/// Uses 30 s per-request timeout, pageSize=200 pagination, and filters for
/// serverless chat/text models with contextLength > 0.
/// Hard limit of 5 pages (~1000 models) and 60 s total wall-clock time.
async fn discover_fireworks_models(api_key: &str) -> Result<Vec<ModelInfo>> {
    let client = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(30))
        .connect_timeout(std::time::Duration::from_secs(10))
        .build()?;

    let mut all_models: Vec<ModelInfo> = Vec::new();
    let mut page_token: Option<String> = None;
    let max_pages = 5;
    let mut page_count = 0;
    let start = std::time::Instant::now();
    let max_duration = std::time::Duration::from_secs(60);

    loop {
        if page_count >= max_pages {
            tracing::info!("Reached max page limit ({}) for Fireworks models", max_pages);
            break;
        }
        if start.elapsed() > max_duration {
            tracing::info!("Reached max duration ({:?}) for Fireworks models", max_duration);
            break;
        }

        let mut url =
            "https://api.fireworks.ai/v1/accounts/fireworks/models?pageSize=200".to_string();
        if let Some(ref token) = page_token {
            url.push_str(&format!("&pageToken={}", token));
        }

        tracing::info!(url = %url, page = page_count + 1, "Fetching Fireworks models page");

        let response = match client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                if e.is_timeout() {
                    tracing::warn!("Fireworks API request timed out after 30s");
                    // If we already have some models from previous pages, return them
                    if !all_models.is_empty() {
                        tracing::info!(count = all_models.len(), "Returning partial results after timeout");
                        return Ok(all_models);
                    }
                    return Err(anyhow::anyhow!(
                        "Fireworks API request timed out after 30s. Check your network connection."
                    ));
                }
                tracing::warn!(error = %e, "Fireworks native models request failed");
                return Err(anyhow::anyhow!(
                    "Failed to reach Fireworks API: {}. Check your network connection and API key.",
                    e
                ));
            }
        };

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(status = %status, body = %body, "Fireworks native API error");
            // If we already have models from previous pages, return partial results
            if !all_models.is_empty() {
                tracing::info!(count = all_models.len(), "Returning partial results after API error");
                return Ok(all_models);
            }
            // Otherwise try the OpenAI-compat fallback
            tracing::info!("Native API failed, trying OpenAI-compatible fallback");
            return discover_fireworks_models_openai_compat(api_key, &client).await;
        }

        let json: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                if !all_models.is_empty() {
                    tracing::info!(count = all_models.len(), "Returning partial results after JSON parse failure");
                    return Ok(all_models);
                }
                tracing::warn!(error = %e, "Failed to parse Fireworks native models response");
                return Err(anyhow::anyhow!(
                    "Failed to parse Fireworks models response: {}",
                    e
                ));
            }
        };

        let model_array = json
            .get("models")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        tracing::info!(
            raw_count = model_array.len(),
            page = page_count + 1,
            "Fireworks native API returned models"
        );

        for m in &model_array {
            let name = m
                .get("name")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            let supports_serverless = m
                .get("supportsServerless")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            if !supports_serverless {
                continue;
            }

            let ctx_len = m
                .get("contextLength")
                .and_then(|v| v.as_u64())
                .unwrap_or(0);
            if ctx_len == 0 {
                continue;
            }

            if !is_fireworks_chat_model(&name) {
                continue;
            }

            let display_name = m
                .get("displayName")
                .and_then(|v| v.as_str())
                .map(String::from);

            let supports_tools = m
                .get("supportsTools")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let supports_vision = m
                .get("supportsImageInput")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);

            let mut desc_parts = vec![format!("{}K ctx", ctx_len / 1024)];
            if supports_tools {
                desc_parts.push("tools".to_string());
            }
            if supports_vision {
                desc_parts.push("vision".to_string());
            }
            let description = Some(desc_parts.join(", "));

            all_models.push(ModelInfo {
                slug: format!("fireworks/{}", name),
                display_name,
                description,
            });
        }

        page_count += 1;

        // Check for next page — break on missing, null, or empty token
        let next_token = json
            .get("nextPageToken")
            .and_then(|v| v.as_str())
            .map(String::from)
            .filter(|t| !t.is_empty());

        if next_token.is_none() || model_array.is_empty() {
            break;
        }
        page_token = next_token;
    }

    tracing::info!(count = all_models.len(), "Discovered Fireworks models via native API");

    if all_models.is_empty() {
        return Err(anyhow::anyhow!(
            "No chat models found in Fireworks account"
        ));
    }

    Ok(all_models)
}

/// Fallback: try the OpenAI-compatible /inference/v1/models endpoint.
/// This endpoint has been known to return 500 errors, so it's only used
/// when the native API fails.
async fn discover_fireworks_models_openai_compat(
    api_key: &str,
    client: &reqwest::Client,
) -> Result<Vec<ModelInfo>> {
    let mut all_models: Vec<ModelInfo> = Vec::new();
    let mut page_token: Option<String> = None;
    let max_pages = 5;
    let mut page_count = 0;

    loop {
        if page_count >= max_pages {
            break;
        }
        let mut url = "https://api.fireworks.ai/inference/v1/models".to_string();
        if let Some(ref token) = page_token {
            url.push_str(&format!("?after={}", token));
        }

        tracing::info!(url = %url, "Fetching Fireworks models (OpenAI-compat fallback)");

        let response = match client
            .get(&url)
            .header("Authorization", format!("Bearer {}", api_key))
            .header("Content-Type", "application/json")
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                tracing::warn!(error = %e, "Fireworks OpenAI-compat models request failed");
                return Err(anyhow::anyhow!(
                    "Failed to reach Fireworks API (both native and OpenAI-compat): {}",
                    e
                ));
            }
        };

        let status = response.status();
        if !status.is_success() {
            let body = response.text().await.unwrap_or_default();
            tracing::warn!(
                status = %status,
                body = %body,
                "Fireworks OpenAI-compat endpoint also failed"
            );
            return Err(anyhow::anyhow!(
                "Fireworks API returned status {} (both endpoints failed)",
                status
            ));
        }

        let json: serde_json::Value = match response.json().await {
            Ok(v) => v,
            Err(e) => {
                return Err(anyhow::anyhow!(
                    "Failed to parse Fireworks OpenAI-compat response: {}",
                    e
                ));
            }
        };

        let model_array = json
            .get("data")
            .and_then(|v| v.as_array())
            .cloned()
            .unwrap_or_default();

        for m in &model_array {
            let raw_id = m
                .get("id")
                .and_then(|v| v.as_str())
                .unwrap_or("unknown")
                .to_string();

            if !is_fireworks_chat_model(&raw_id) {
                continue;
            }

            let display_name = m
                .get("name")
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let description = m
                .get("description")
                .and_then(|v| v.as_str())
                .map(String::from);

            all_models.push(ModelInfo {
                slug: format!("fireworks/{}", raw_id),
                display_name,
                description,
            });
        }

        page_count += 1;

        let has_more = json
            .get("has_more")
            .and_then(|v| v.as_bool())
            .unwrap_or(false);

        if !has_more || model_array.is_empty() {
            break;
        }

        if let Some(last) = model_array.last() {
            let next = last.get("id").and_then(|v| v.as_str()).map(String::from);
            if next.as_ref().map_or(true, |t| t.is_empty()) {
                break;
            }
            page_token = next;
        } else {
            break;
        }
    }

    tracing::info!(
        count = all_models.len(),
        "Discovered Fireworks models via OpenAI-compat fallback"
    );

    if all_models.is_empty() {
        return Err(anyhow::anyhow!(
            "No chat models found in Fireworks account"
        ));
    }

    Ok(all_models)
}

/// Minimal fallback list of Fireworks models used only when the API is
/// completely unreachable (no network, invalid key, etc.).
fn discover_known_fireworks_models() -> Result<Vec<ModelInfo>> {
    let known_models = vec![
        (
            "accounts/fireworks/models/deepseek-v3p1",
            "DeepSeek V3.1",
            "Strong general-purpose model (fallback)",
        ),
        (
            "accounts/fireworks/models/llama-v3p3-70b-instruct",
            "Llama 3.3 70B Instruct",
            "Meta Llama 3.3 70B (fallback)",
        ),
    ];

    tracing::warn!(
        "Using hardcoded Fireworks model list — live discovery failed. \
         Models may not reflect your account's available models."
    );

    Ok(known_models
        .into_iter()
        .map(|(slug, name, desc)| ModelInfo {
            slug: format!("fireworks/{}", slug),
            display_name: Some(name.to_string()),
            description: Some(desc.to_string()),
        })
        .collect())
}

/// Check if a Fireworks model ID is a chat/instruct/text model suitable for
/// agent use.  Excludes embedding, reranker, image-generation, audio, and
/// other non-chat model families.
///
/// When using the native API, the `supportsServerless` and `contextLength`
/// filters already narrow down to inference-ready models.  This filter
/// further excludes names that are clearly not text-generation models.
fn is_fireworks_chat_model(model_id: &str) -> bool {
    // Exclude known non-chat model families
    if model_id.contains("embedding")
        || model_id.contains("-embed-")
        || model_id.contains("reranker")
        || model_id.contains("-rerank-")
        || model_id.contains("whisper")
        || model_id.contains("tts")
        || model_id.contains("dall-e")
        || model_id.contains("-audio-")
        || model_id.contains("flux-")
        || model_id.contains("stable-diffusion")
        || model_id.contains("sd3-")
        || model_id.contains("clip-")
    {
        return false;
    }

    // Accept all models under accounts/fireworks/models/
    model_id.starts_with("accounts/fireworks/models/")
}

/// Minimal fallback list of Anthropic models used only when both the API
/// and the CLI are completely unreachable.
fn discover_known_anthropic_models() -> Result<Vec<ModelInfo>> {
    let known_models = vec![
        (
            "claude-sonnet-4-5",
            "Claude Sonnet 4.5",
            "Latest Sonnet (fallback)",
        ),
        (
            "claude-3-5-sonnet-20241022",
            "Claude 3.5 Sonnet",
            "Stable Sonnet (fallback)",
        ),
    ];

    Ok(known_models
        .into_iter()
        .map(|(slug, name, desc)| ModelInfo {
            slug: format!("anthropic/{}", slug),
            display_name: Some(name.to_string()),
            description: Some(desc.to_string()),
        })
        .collect())
}

/// Minimal fallback list of OpenAI models used only when both the Codex CLI
/// and the OpenAI API are completely unreachable.
fn discover_known_openai_models() -> Result<Vec<ModelInfo>> {
    let known_models = vec![
        ("gpt-4o", "GPT-4o", "Latest GPT-4o (fallback)"),
        ("gpt-4o-mini", "GPT-4o Mini", "Fast, cost-effective (fallback)"),
    ];

    Ok(known_models
        .into_iter()
        .map(|(slug, name, desc)| ModelInfo {
            slug: format!("openai/{}", slug),
            display_name: Some(name.to_string()),
            description: Some(desc.to_string()),
        })
        .collect())
}

/// Get default model for a CLI backend.
/// All identifiers use a `provider/` prefix so the fallback client chain
/// can route to the correct provider without extra env vars.
pub fn default_model_for_backend(cli_backend: &str) -> &'static str {
    match cli_backend {
        "codex" => "openai/gpt-4o",
        _ => "openai/gpt-4o",
    }
}

/// Get default model for a provider.
pub fn default_model_for_provider(provider: &str) -> &'static str {
    if provider.contains("Anthropic") {
        // Claude 3.5 Sonnet - known working model
        "anthropic/claude-3-5-sonnet-20241022"
    } else if provider.contains("OpenAI") || provider.contains("Codex") {
        "openai/gpt-4o"
    } else if provider.contains("Fireworks") {
        "fireworks/accounts/fireworks/models/deepseek-v3p1"
    } else {
        "openai/gpt-4o"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_model_for_backend() {
        assert_eq!(default_model_for_backend("codex"), "openai/gpt-4o");
        assert_eq!(default_model_for_backend("unknown"), "openai/gpt-4o");
    }

    #[test]
    fn test_default_model_for_provider() {
        assert_eq!(
            default_model_for_provider("Anthropic"),
            "anthropic/claude-3-5-sonnet-20241022"
        );
        assert_eq!(default_model_for_provider("OpenAI"), "openai/gpt-4o");
        assert_eq!(default_model_for_provider("Codex"), "openai/gpt-4o");
        assert_eq!(
            default_model_for_provider("Fireworks"),
            "fireworks/accounts/fireworks/models/deepseek-v3p1"
        );
        assert_eq!(default_model_for_provider("Unknown"), "openai/gpt-4o");
    }

    #[test]
    fn test_map_claude_model_alias() {
        assert_eq!(
            map_claude_model_alias("claude-3-5-sonnet-latest"),
            "claude-3-5-sonnet-20241022"
        );
        assert_eq!(
            map_claude_model_alias("claude-3-5-haiku-latest"),
            "claude-3-5-haiku-20241022"
        );
        assert_eq!(
            map_claude_model_alias("claude-3-opus-latest"),
            "claude-3-opus-20240229"
        );
        assert_eq!(
            map_claude_model_alias("claude-3-5-sonnet-20241022"),
            "claude-3-5-sonnet-20241022"
        ); // already dated
    }

    #[test]
    fn test_is_anthropic_chat_model() {
        // Valid API model IDs (dated versions)
        assert!(is_anthropic_chat_model("claude-3-5-sonnet-20241022"));
        assert!(is_anthropic_chat_model("claude-3-opus-20240229"));
        assert!(is_anthropic_chat_model("claude-3-5-haiku-20241022"));
        assert!(is_anthropic_chat_model("claude-3-haiku-20240307"));
        // Valid: newer versioned IDs returned by /v1/models
        assert!(is_anthropic_chat_model("claude-opus-4-6"));
        assert!(is_anthropic_chat_model("claude-sonnet-4-5"));
        // Invalid: -latest aliases
        assert!(!is_anthropic_chat_model("claude-3-7-sonnet-latest"));
        assert!(!is_anthropic_chat_model("claude-3-5-sonnet-latest"));
        // Invalid: RLHF models
        assert!(!is_anthropic_chat_model("claude-3-5-sonnet-rlhf-20241022"));
        // Invalid: eval models
        assert!(!is_anthropic_chat_model("claude-3-5-sonnet-eval-20241022"));
    }

    #[test]
    fn test_is_valid_anthropic_api_model() {
        // Valid: dated versions with YYYYMMDD
        assert!(is_valid_anthropic_api_model("claude-3-5-sonnet-20241022"));
        assert!(is_valid_anthropic_api_model("claude-3-opus-20240229"));
        assert!(is_valid_anthropic_api_model("claude-3-5-haiku-20241022"));
        assert!(is_valid_anthropic_api_model("claude-3-haiku-20240307"));
        // Valid: newer versioned IDs returned by /v1/models
        assert!(is_valid_anthropic_api_model("claude-opus-4-6"));
        assert!(is_valid_anthropic_api_model("claude-sonnet-4-5"));
        // Invalid: -latest aliases
        assert!(!is_valid_anthropic_api_model("claude-3-7-sonnet-latest"));
        assert!(!is_valid_anthropic_api_model("claude-3-5-sonnet-latest"));
        // Invalid: too short
        assert!(!is_valid_anthropic_api_model("claude-3"));
        assert!(!is_valid_anthropic_api_model("claude"));
    }

    #[test]
    fn test_is_openai_chat_model() {
        assert!(is_openai_chat_model("gpt-4o"));
        assert!(is_openai_chat_model("gpt-4"));
        assert!(is_openai_chat_model("o1-preview"));
        assert!(!is_openai_chat_model("text-embedding-3-small"));
        assert!(!is_openai_chat_model("whisper-1"));
        assert!(!is_openai_chat_model("dall-e-3"));
    }

    #[test]
    fn test_is_fireworks_chat_model() {
        // Fireworks serverless models are accepted by prefix
        assert!(is_fireworks_chat_model(
            "accounts/fireworks/models/llama-v3p1-8b-instruct"
        ));
        assert!(is_fireworks_chat_model(
            "accounts/fireworks/models/deepseek-v3p1"
        ));
        assert!(is_fireworks_chat_model(
            "accounts/fireworks/models/kimi-k2p6"
        ));
        assert!(is_fireworks_chat_model(
            "accounts/fireworks/models/qwen3-235b-a22b"
        ));
        assert!(is_fireworks_chat_model(
            "accounts/fireworks/models/glm-5p1"
        ));
        // Non-chat models are excluded
        assert!(!is_fireworks_chat_model(
            "accounts/fireworks/models/qwen3-embedding-8b"
        ));
        assert!(!is_fireworks_chat_model(
            "accounts/fireworks/models/qwen3-reranker-8b"
        ));
        assert!(!is_fireworks_chat_model(
            "accounts/fireworks/models/flux-1-schnell-fp8"
        ));
        assert!(!is_fireworks_chat_model(
            "accounts/fireworks/models/whisper-large"
        ));
        // Models without the Fireworks prefix are not accepted
        assert!(!is_fireworks_chat_model("some-random-model"));
        assert!(!is_fireworks_chat_model("embedding-model"));
    }

    #[test]
    fn test_discover_known_fireworks_models() {
        let models = discover_known_fireworks_models().unwrap();
        assert!(!models.is_empty());
        assert!(models.len() >= 2, "Fallback should have at least 2 models");
        for m in &models {
            assert!(
                m.slug.starts_with("fireworks/"),
                "Expected fireworks/ prefix, got: {}",
                m.slug
            );
            assert!(m.display_name.is_some(), "Expected display_name for {}", m.slug);
        }
        assert_eq!(
            models[0].slug,
            "fireworks/accounts/fireworks/models/deepseek-v3p1"
        );
    }

    #[test]
    fn test_discover_known_anthropic_models() {
        let models = discover_known_anthropic_models().unwrap();
        assert!(!models.is_empty());
        assert!(models.len() >= 2, "Fallback should have at least 2 models");
        for m in &models {
            assert!(
                m.slug.starts_with("anthropic/"),
                "Expected anthropic/ prefix, got: {}",
                m.slug
            );
            assert!(m.display_name.is_some(), "Expected display_name for {}", m.slug);
        }
    }

    #[test]
    fn test_discover_known_openai_models() {
        let models = discover_known_openai_models().unwrap();
        assert!(!models.is_empty());
        assert!(models.len() >= 2, "Fallback should have at least 2 models");
        for m in &models {
            assert!(
                m.slug.starts_with("openai/"),
                "Expected openai/ prefix, got: {}",
                m.slug
            );
            assert!(m.display_name.is_some(), "Expected display_name for {}", m.slug);
        }
    }
}
