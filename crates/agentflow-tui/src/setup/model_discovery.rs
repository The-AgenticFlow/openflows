use anyhow::Result;
use std::process::Command;

use super::SetupConfig;

#[derive(Debug, Clone)]
pub struct ModelInfo {
    pub slug: String,
    pub display_name: Option<String>,
    pub description: Option<String>,
}

/// Discover available models from the selected provider / CLI backend.
/// If a custom provider is configured, queries its /v1/models endpoint.
pub async fn discover_models(config: &SetupConfig) -> Result<Vec<ModelInfo>> {
    if let Some(p) = config.selected_provider.as_deref() {
        if p.contains("Custom Provider") {
            if let Some(ref url) = config.gateway_url {
                return discover_custom_provider_models(url, config.gateway_api_key.as_deref()).await;
            }
        }
    }
    discover_backend_models(&config.selected_cli_backend)
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

    // Fallback to known-valid model IDs (using -latest aliases where possible)
    let known_models = vec![
        ("claude-sonnet-4-5", "Claude Sonnet 4.5", "Latest Claude Sonnet model"),
        ("claude-opus-4-5", "Claude Opus 4.5", "Latest Claude Opus model"),
        ("claude-3-7-sonnet-latest", "Claude 3.7 Sonnet", "Claude 3.7 Sonnet model"),
        ("claude-3-5-sonnet-latest", "Claude 3.5 Sonnet", "Previous generation Sonnet"),
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
        models.push(ModelInfo {
            slug: format!("anthropic/{}", line),
            display_name: Some(line.replace('-', " ").to_string()),
            description: None,
        });
    }

    if models.is_empty() {
        return Err(anyhow::anyhow!("no models found in claude CLI output"));
    }

    Ok(models)
}

/// Get default model for a CLI backend.
/// All identifiers use a `provider/` prefix so the fallback client chain
/// can route to the correct provider without extra env vars.
pub fn default_model_for_backend(cli_backend: &str) -> &'static str {
    match cli_backend {
        "codex" => "openai/gpt-5.5",
        "claude" => "anthropic/claude-sonnet-4-5",
        _ => "anthropic/claude-sonnet-4-5",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_model_for_backend() {
        assert_eq!(default_model_for_backend("codex"), "openai/gpt-5.5");
        assert_eq!(default_model_for_backend("claude"), "anthropic/claude-sonnet-4-5");
        assert_eq!(default_model_for_backend("unknown"), "anthropic/claude-sonnet-4-5");
    }

    #[test]
    fn test_discover_claude_models() {
        let models = discover_claude_models().unwrap();
        assert!(!models.is_empty());
        assert!(models[0].slug.starts_with("anthropic/"));
    }
}
