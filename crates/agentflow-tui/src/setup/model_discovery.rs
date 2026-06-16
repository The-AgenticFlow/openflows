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
/// Fetches models dynamically from Anthropic, OpenAI, or Fireworks based on the selected provider.
pub async fn discover_models(config: &SetupConfig) -> Result<Vec<ModelInfo>> {
    match config.selected_provider.as_deref() {
        Some(p) if p.contains("Anthropic") => {
            // For Anthropic, use the Claude CLI to discover models
            discover_claude_models()
        }
        Some(p) if p.contains("OpenAI") || p.contains("Codex") => {
            // For OpenAI/Codex, try Codex CLI first, then fall back to OpenAI API
            if let Ok(models) = discover_codex_models() {
                if !models.is_empty() {
                    return Ok(models);
                }
            }
            // Fallback to OpenAI API if Codex CLI doesn't work
            if let Some(ref key) = config.openai_key {
                return discover_openai_models(key).await;
            }
            Err(anyhow::anyhow!("No OpenAI API key configured"))
        }
        Some(p) if p.contains("Fireworks") => {
            if let Some(ref key) = config.fireworks_key {
                return discover_fireworks_models(key).await;
            }
            Err(anyhow::anyhow!("No Fireworks API key configured"))
        }
        _ => discover_backend_models(&config.selected_cli_backend),
    }
}

fn discover_backend_models(cli_backend: &str) -> Result<Vec<ModelInfo>> {
    match cli_backend {
        "codex" => discover_codex_models(),
        "claude" => discover_claude_models(),
        other => Err(anyhow::anyhow!("Unknown CLI backend: {}", other)),
    }
}

/// Query a custom (OpenAI-compatible) provider for available models.
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
    let output = Command::new("codex")
        .args(["debug", "models"])
        .output()?;

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
            let raw_slug = m.get("slug")
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

            let display_name = m.get("display_name")
                .or_else(|| m.get("name"))
                .and_then(|v| v.as_str())
                .map(String::from);

            let description = m.get("description")
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

/// Fetch models from claude CLI
fn discover_claude_models() -> Result<Vec<ModelInfo>> {
    // Try live discovery from claude CLI first
    if let Ok(models) = discover_claude_models_from_cli() {
        if !models.is_empty() {
            return Ok(models);
        }
    }

    // Fallback to known-valid Anthropic API model IDs
    // Note: -latest aliases don't work with the API, must use dated versions
    let known_models = vec![
        ("claude-3-7-sonnet-20250219", "Claude 3.7 Sonnet", "Latest Claude 3.7 Sonnet"),
        ("claude-3-5-sonnet-20241022", "Claude 3.5 Sonnet", "Claude 3.5 Sonnet"),
        ("claude-3-5-haiku-20241022", "Claude 3.5 Haiku", "Fast, cost-effective"),
        ("claude-3-opus-20240229", "Claude 3 Opus", "Powerful model for complex tasks"),
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

/// Query claude CLI for available models
/// Maps CLI -latest aliases to actual API model IDs
fn discover_claude_models_from_cli() -> Result<Vec<ModelInfo>> {
    let output = Command::new("claude")
        .args(["models"])
        .output()?;

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
        "claude-3-7-sonnet-latest" => "claude-3-7-sonnet-20250219",
        "claude-3-5-sonnet-latest" => "claude-3-5-sonnet-20241022",
        "claude-3-5-haiku-latest" => "claude-3-5-haiku-20241022",
        "claude-3-opus-latest" => "claude-3-opus-20240229",
        // If it's already a dated version or unknown, return as-is
        _ => alias,
    }
}

/// Fetch available models from OpenAI API.
async fn discover_openai_models(api_key: &str) -> Result<Vec<ModelInfo>> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://api.openai.com/v1/models")
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "OpenAI API returned status {} when listing models",
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

            // Skip non-chat models and embeddings
            if !is_openai_chat_model(&raw_id) {
                continue;
            }

            let display_name = m
                .get("name")
                .or_else(|| m.get("id"))
                .and_then(|v| v.as_str())
                .map(String::from);

            models.push(ModelInfo {
                slug: format!("openai/{}", raw_id),
                display_name,
                description: None,
            });
        }
    }

    if models.is_empty() {
        return Err(anyhow::anyhow!("No chat models found in OpenAI account"));
    }

    Ok(models)
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

/// Fetch available models from Fireworks API.
async fn discover_fireworks_models(api_key: &str) -> Result<Vec<ModelInfo>> {
    let client = reqwest::Client::new();
    let response = client
        .get("https://api.fireworks.ai/inference/v1/models")
        .header("Authorization", format!("Bearer {}", api_key))
        .send()
        .await?;

    if !response.status().is_success() {
        return Err(anyhow::anyhow!(
            "Fireworks API returned status {} when listing models",
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

            // Skip non-chat models
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

            models.push(ModelInfo {
                slug: format!("fireworks/{}", raw_id),
                display_name,
                description,
            });
        }
    }

    if models.is_empty() {
        return Err(anyhow::anyhow!("No chat models found in Fireworks account"));
    }

    Ok(models)
}

/// Check if a Fireworks model ID is a chat model
fn is_fireworks_chat_model(model_id: &str) -> bool {
    // Fireworks chat models typically have these patterns
    model_id.contains("instruct")
        || model_id.contains("chat")
        || model_id.starts_with("accounts/")
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
        "anthropic/claude-3-7-sonnet-20250219"
    } else if provider.contains("OpenAI") || provider.contains("Codex") {
        "openai/gpt-4o"
    } else if provider.contains("Fireworks") {
        "fireworks/accounts/fireworks/models/llama-v3p1-8b-instruct"
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
        assert_eq!(default_model_for_provider("Anthropic"), "anthropic/claude-3-7-sonnet-20250219");
        assert_eq!(default_model_for_provider("OpenAI"), "openai/gpt-4o");
        assert_eq!(default_model_for_provider("Codex"), "openai/gpt-4o");
        assert_eq!(default_model_for_provider("Fireworks"), "fireworks/accounts/fireworks/models/llama-v3p1-8b-instruct");
        assert_eq!(default_model_for_provider("Unknown"), "openai/gpt-4o");
    }

    #[test]
    fn test_map_claude_model_alias() {
        assert_eq!(map_claude_model_alias("claude-3-7-sonnet-latest"), "claude-3-7-sonnet-20250219");
        assert_eq!(map_claude_model_alias("claude-3-5-sonnet-latest"), "claude-3-5-sonnet-20241022");
        assert_eq!(map_claude_model_alias("claude-3-5-haiku-latest"), "claude-3-5-haiku-20241022");
        assert_eq!(map_claude_model_alias("claude-3-opus-latest"), "claude-3-opus-20240229");
        assert_eq!(map_claude_model_alias("claude-3-7-sonnet-20250219"), "claude-3-7-sonnet-20250219"); // already dated
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
        assert!(is_fireworks_chat_model("accounts/fireworks/models/llama-v3p1-8b-instruct"));
        assert!(!is_fireworks_chat_model("some-embedding-model"));
    }
}
