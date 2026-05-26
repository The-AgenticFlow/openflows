// crates/agent-client/src/fallback.rs
//
// FallbackClient — tries multiple LLM providers in order, falling back on failure.
//
// Use this when you want automatic failover between providers (e.g., Gemini -> Claude).
//
// ## Proxy Mode
// When PROXY_URL is set, the client uses the proxy exclusively and doesn't require
// individual API keys. The proxy handles routing to the correct backend.
//
// ## Direct Mode
// When PROXY_URL is not set, individual API keys are required for each provider.

use anyhow::{bail, Result};
use async_trait::async_trait;
use std::time::Duration;
use tracing::{info, warn};

use crate::anthropic::AnthropicClient;
use crate::fireworks::FireworksClient;
use crate::gemini::GeminiClient;
use crate::openai::OpenAiClient;
use crate::types::{LlmClient, LlmResponse, Message, ToolSchema};

/// Check if a proxy is configured.
fn proxy_is_configured() -> bool {
    std::env::var("PROXY_URL").is_ok() || std::env::var("ANTHROPIC_BASE_URL").is_ok()
}

/// Check if an external connector (gateway) is configured.
/// When true, direct API fallbacks should NOT be used - only the connector.
fn external_connector_is_configured() -> bool {
    std::env::var("GATEWAY_API_KEY").is_ok() || std::env::var("FIREWORKS_API_KEY").is_ok()
}

/// Resolve provider for a model based on MODEL_PROVIDER_MAP.
fn resolve_provider_for_model(model: &str) -> Option<String> {
    let map = std::env::var("MODEL_PROVIDER_MAP").ok()?;
    for entry in map.split(',') {
        let entry = entry.trim();
        if let Some((prefix, provider)) = entry.split_once('=') {
            if model.starts_with(prefix.trim()) {
                return Some(provider.trim().to_string());
            }
        }
    }
    None
}

/// Check if an API key is available for a provider.
fn has_api_key_for_provider(provider: &str) -> bool {
    match provider {
        "anthropic" => std::env::var("ANTHROPIC_API_KEY").is_ok(),
        "openai" => std::env::var("OPENAI_API_KEY").is_ok(),
        "gemini" => std::env::var("GEMINI_API_KEY").is_ok(),
        "fireworks" => std::env::var("FIREWORKS_API_KEY").is_ok(),
        _ => false,
    }
}

pub struct FallbackClient {
    clients: Vec<Box<dyn LlmClient>>,
    current_idx: usize,
    timeout: Duration,
    max_retries: u32,
    retry_delay_ms: u64,
}

impl FallbackClient {
    pub fn new(clients: Vec<Box<dyn LlmClient>>, timeout: Duration) -> Self {
        let max_retries = std::env::var("LLM_MAX_RETRIES")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(3);
        let retry_delay_ms = std::env::var("LLM_RETRY_DELAY_MS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(1000);
        Self {
            clients,
            current_idx: 0,
            timeout,
            max_retries,
            retry_delay_ms,
        }
    }

    pub fn from_env() -> Result<Self> {
        Self::build(None)
    }

    pub fn from_env_with_model(model_override: &str) -> Result<Self> {
        Self::build(Some(model_override))
    }

    fn build(model_override: Option<&str>) -> Result<Self> {
        let proxy_active = proxy_is_configured();
        let fireworks_active = FireworksClient::is_configured();

        info!(
            proxy_active,
            fireworks_active, model_override, "Building fallback client chain"
        );

        if let Some(model) = model_override {
            if let Some(provider) = resolve_provider_for_model(model) {
                match provider.as_str() {
                    "fireworks" if FireworksClient::is_configured() => {
                        info!(
                            model,
                            provider = "fireworks",
                            "Model-aware routing: using FireworksClient directly"
                        );
                        return Self::build_fireworks_chain(Some(model));
                    }
                    "openai" if has_api_key_for_provider("openai") => {
                        info!(
                            model,
                            provider = "openai",
                            "Model-aware routing: using OpenAiClient directly"
                        );
                        return Self::build_openai_direct_chain(model);
                    }
                    "anthropic"
                        if has_api_key_for_provider("anthropic") && !proxy_is_configured() =>
                    {
                        info!(
                            model,
                            provider = "anthropic",
                            "Model-aware routing: using AnthropicClient directly"
                        );
                        return Self::build_anthropic_direct_chain(model);
                    }
                    _ => {}
                }
            }

            if model.starts_with("accounts/fireworks/") && FireworksClient::is_configured() {
                info!(
                    model,
                    "Auto-detected Fireworks model from prefix — using FireworksClient directly"
                );
                return Self::build_fireworks_chain(Some(model));
            }
        }

        if proxy_active {
            return Self::build_proxy_chain(model_override);
        }

        if fireworks_active {
            return Self::build_fireworks_chain(model_override);
        }

        Self::build_direct_chain(model_override)
    }

    fn build_fireworks_chain(model_override: Option<&str>) -> Result<Self> {
        let mut clients: Vec<Box<dyn LlmClient>> = Vec::new();
        let model = model_override.unwrap_or("accounts/fireworks/models/llama-v3p1-8b-instruct");

        info!(
            model = model,
            "Fireworks mode: configuring client (no direct fallbacks - external connector)"
        );

        if let Ok(c) = FireworksClient::from_env_with_model(model) {
            info!(provider = "fireworks", model = %c.model(), "Fireworks client initialized");
            clients.push(Box::new(c));
        }

        // When Fireworks is configured directly, it IS the external connector.
        // Do NOT add direct API fallbacks - the connector is authoritative.
        // Priority: PROXY > GATEWAY/FIREWORKS > direct APIs (only when no connector).

        if clients.is_empty() {
            bail!(
                "Fireworks mode: Failed to initialize Fireworks client. \
                 Ensure FIREWORKS_API_KEY is set."
            );
        }

        let timeout_secs = std::env::var("LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);

        Ok(Self::new(clients, Duration::from_secs(timeout_secs)))
    }

    fn build_openai_direct_chain(model: &str) -> Result<Self> {
        let mut clients: Vec<Box<dyn LlmClient>> = Vec::new();
        if let Ok(c) = OpenAiClient::from_env_with_model(model) {
            info!(provider = "openai-direct", model = %c.model(), "Direct OpenAI client initialized");
            clients.push(Box::new(c));
        }
        if clients.is_empty() {
            bail!("OpenAI direct mode: OPENAI_API_KEY not set");
        }
        let timeout_secs = std::env::var("LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);
        Ok(Self::new(clients, Duration::from_secs(timeout_secs)))
    }

    fn build_anthropic_direct_chain(model: &str) -> Result<Self> {
        let mut clients: Vec<Box<dyn LlmClient>> = Vec::new();
        if let Ok(c) = AnthropicClient::from_env_with_model(model) {
            info!(provider = "anthropic-direct", model = %c.model(), "Direct Anthropic client initialized");
            clients.push(Box::new(c));
        }
        if clients.is_empty() {
            bail!("Anthropic direct mode: ANTHROPIC_API_KEY not set");
        }
        let timeout_secs = std::env::var("LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);
        Ok(Self::new(clients, Duration::from_secs(timeout_secs)))
    }

    fn build_proxy_chain(model_override: Option<&str>) -> Result<Self> {
        let mut clients: Vec<Box<dyn LlmClient>> = Vec::new();
        let model = model_override.unwrap_or("claude-haiku-4-5-20251001");

        let mapped_provider = model_override.and_then(resolve_provider_for_model);
        let external_connector = external_connector_is_configured();

        info!(
            model = model,
            mapped_provider = ?mapped_provider,
            external_connector = external_connector,
            "Proxy mode: configuring client{}",
            if external_connector { " (external connector - no direct fallbacks)" } else { " (with direct-key fallbacks)" }
        );

        match mapped_provider.as_deref() {
            Some("openai") => match OpenAiClient::from_proxy(model) {
                Ok(c) => {
                    info!(provider = "openai-proxy", model = %c.model(), "Proxy client initialized");
                    clients.push(Box::new(c));
                }
                Err(e) => {
                    warn!(provider = "openai-proxy", error = %e, "Failed to init proxy client")
                }
            },
            Some("gemini") => {
                warn!("Gemini proxy format not yet supported, falling back to Anthropic format");
            }
            _ => match AnthropicClient::from_env_with_model(model) {
                Ok(c) => {
                    info!(provider = "anthropic-proxy", model = %c.model(), "Proxy client initialized");
                    clients.push(Box::new(c));
                }
                Err(e) => {
                    warn!(provider = "anthropic-proxy", error = %e, "Failed to init proxy client")
                }
            },
        }

        // --- 2. Direct-key fallbacks ---
        // When an external connector (Fireworks/Gateway) is configured, do NOT add
        // direct API fallbacks. The connector is the authoritative provider.
        // Priority: PROXY > GATEWAY > direct APIs (only when connector is absent).
        if external_connector {
            info!("External connector configured - skipping direct API fallbacks");
        } else {
            let skip_anthropic_direct =
                matches!(mapped_provider.as_deref(), None | Some("anthropic"));
            if !skip_anthropic_direct {
                if let Ok(c) = AnthropicClient::from_env_with_model(model) {
                    info!(provider = "anthropic-direct", model = %c.model(), "Direct fallback initialized");
                    clients.push(Box::new(c));
                }
            }

            if std::env::var("GEMINI_API_KEY").is_ok() {
                if let Ok(c) = GeminiClient::from_env() {
                    info!(provider = "gemini-direct", model = %c.model(), "Direct fallback initialized");
                    clients.push(Box::new(c));
                }
            }

            if std::env::var("OPENAI_API_KEY").is_ok() {
                if matches!(mapped_provider.as_deref(), Some("openai")) {
                    // Already using OpenAI-format proxy — skip direct OpenAI
                } else if let Ok(c) = OpenAiClient::from_env() {
                    info!(provider = "openai-direct", model = %c.model(), "Direct fallback initialized");
                    clients.push(Box::new(c));
                }
            }
        }

        if clients.is_empty() {
            bail!(
                "Proxy mode: Failed to initialize any client. \
                 Ensure PROXY_URL is set and your proxy is running, \
                 or set at least one direct API key (ANTHROPIC_API_KEY, \
                 GEMINI_API_KEY, OPENAI_API_KEY) as fallback."
            );
        }

        let timeout_secs = std::env::var("LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(120);

        Ok(Self::new(clients, Duration::from_secs(timeout_secs)))
    }

    /// Build client chain for direct mode.
    /// Requires individual API keys for each provider.
    fn build_direct_chain(model_override: Option<&str>) -> Result<Self> {
        let fallback_order =
            std::env::var("LLM_FALLBACK").unwrap_or_else(|_| "anthropic,gemini,openai".to_string());

        let provider_names: Vec<&str> = fallback_order.split(',').map(|s| s.trim()).collect();

        let mut clients: Vec<Box<dyn LlmClient>> = Vec::new();

        // If model is mapped to a specific provider, prepend it to the chain
        let mapped_provider = model_override.and_then(resolve_provider_for_model);
        if let Some(ref provider) = mapped_provider {
            if !provider_names.contains(&provider.as_str()) {
                info!(
                    model = model_override.unwrap_or(""),
                    mapped_provider = %provider,
                    "Model mapped to specific provider"
                );
                if let Some(client) = Self::try_init_provider(provider, model_override) {
                    clients.push(client);
                }
            }
        }

        // Add providers from fallback order
        for name in provider_names {
            // Skip if this provider was already added via mapping
            if mapped_provider.as_deref() == Some(name) && !clients.is_empty() {
                continue;
            }

            if let Some(client) = Self::try_init_provider(name, model_override) {
                clients.push(client);
            }
        }

        if clients.is_empty() {
            bail!(
                "Direct mode: No valid LLM providers configured. \
                 Set at least one of: ANTHROPIC_API_KEY, GEMINI_API_KEY, or OPENAI_API_KEY. \
                 Alternatively, set PROXY_URL to use a LiteLLM proxy."
            );
        }

        let timeout_secs = std::env::var("LLM_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(60);

        Ok(Self::new(clients, Duration::from_secs(timeout_secs)))
    }

    /// Try to initialize a provider, returning None on failure.
    fn try_init_provider(name: &str, model_override: Option<&str>) -> Option<Box<dyn LlmClient>> {
        // Check if API key is available
        if !has_api_key_for_provider(name) {
            warn!(provider = name, "Skipping provider - API key not set");
            return None;
        }

        let result: Option<Box<dyn LlmClient>> =
            match name {
                "proxy" => {
                    let client = match model_override {
                        Some(m) => AnthropicClient::from_env_with_model(m),
                        None => AnthropicClient::from_env(),
                    };
                    client.map(|c| {
                    info!(provider = name, model = %c.model(), "Client initialized (proxy)");
                    Box::new(c) as Box<dyn LlmClient>
                }).ok()
                }
                "openai-proxy" => {
                    let default_model =
                        std::env::var("OPENAI_MODEL").unwrap_or_else(|_| "gpt-4o-mini".to_string());
                    let model = model_override.unwrap_or(&default_model);
                    OpenAiClient::from_proxy(model).map(|c| {
                    info!(provider = name, model = %c.model(), "Client initialized (openai-proxy)");
                    Box::new(c) as Box<dyn LlmClient>
                }).ok()
                }
                "anthropic" => {
                    let client = match model_override {
                        Some(m) => AnthropicClient::from_env_with_model(m),
                        None => AnthropicClient::from_env(),
                    };
                    client
                        .map(|c| {
                            info!(provider = name, model = %c.model(), "Client initialized");
                            Box::new(c) as Box<dyn LlmClient>
                        })
                        .ok()
                }
                "gemini" => GeminiClient::from_env()
                    .map(|c| {
                        info!(provider = name, model = %c.model(), "Client initialized");
                        Box::new(c) as Box<dyn LlmClient>
                    })
                    .ok(),
                "openai" => {
                    let client = match model_override {
                        Some(m) => OpenAiClient::from_env_with_model(m),
                        None => OpenAiClient::from_env(),
                    };
                    client
                        .map(|c| {
                            info!(provider = name, model = %c.model(), "Client initialized");
                            Box::new(c) as Box<dyn LlmClient>
                        })
                        .ok()
                }
                "fireworks" => {
                    let client = match model_override {
                        Some(m) => FireworksClient::from_env_with_model(m),
                        None => FireworksClient::from_env(),
                    };
                    client
                        .map(|c| {
                            info!(provider = name, model = %c.model(), "Client initialized");
                            Box::new(c) as Box<dyn LlmClient>
                        })
                        .ok()
                }
                other => {
                    warn!(provider = other, "Unknown provider, skipping");
                    None
                }
            };

        if let Some(ref client) = result {
            info!(provider = name, model = %client.model(), "Provider initialized successfully");
        }

        result
    }
}

#[async_trait]
impl LlmClient for FallbackClient {
    async fn send(&self, messages: &[Message], tools: &[ToolSchema]) -> Result<LlmResponse> {
        let mut last_error = None;

        for (idx, client) in self.clients.iter().enumerate() {
            if idx > 0 {
                warn!(
                    from_provider = self
                        .clients
                        .get(idx - 1)
                        .map(|c| c.model())
                        .unwrap_or("unknown"),
                    to_provider = client.model(),
                    "Falling back to next provider"
                );
            }

            // Retry with exponential backoff
            for attempt in 0..self.max_retries {
                let result = tokio::time::timeout(self.timeout, client.send(messages, tools)).await;

                match result {
                    Ok(Ok(response)) => {
                        if attempt > 0 {
                            info!(
                                provider = client.model(),
                                attempt = attempt + 1,
                                "Request succeeded after retry"
                            );
                        }
                        return Ok(response);
                    }
                    Ok(Err(e)) => {
                        let error_str = e.to_string();
                        let is_retryable = error_str.contains("504")
                            || error_str.contains("502")
                            || error_str.contains("503")
                            || error_str.contains("429")
                            || error_str.contains("timeout")
                            || error_str.contains("Gateway")
                            || error_str.contains("HTTP request")
                            || error_str.contains("connection")
                            || error_str.contains("connect");

                        if is_retryable && attempt < self.max_retries - 1 {
                            let delay_ms = self.retry_delay_ms * (2_u64.pow(attempt));
                            warn!(
                                provider = client.model(),
                                attempt = attempt + 1,
                                max_retries = self.max_retries,
                                delay_ms = delay_ms,
                                error = %e,
                                "Retryable error, retrying with exponential backoff"
                            );
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                            continue;
                        }
                        warn!(
                            provider = client.model(),
                            error = %e,
                            "Provider failed, trying next"
                        );
                        last_error = Some(e);
                        break;
                    }
                    Err(_timeout) => {
                        if attempt < self.max_retries - 1 {
                            let delay_ms = self.retry_delay_ms * (2_u64.pow(attempt));
                            warn!(
                                provider = client.model(),
                                attempt = attempt + 1,
                                max_retries = self.max_retries,
                                delay_ms = delay_ms,
                                timeout_secs = self.timeout.as_secs(),
                                "Provider timed out, retrying with exponential backoff"
                            );
                            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
                            continue;
                        }
                        warn!(
                            provider = client.model(),
                            timeout_secs = self.timeout.as_secs(),
                            "Provider timed out after all retries, trying next"
                        );
                        last_error = Some(anyhow::anyhow!(
                            "Provider timed out after {}s ({} retries exhausted)",
                            self.timeout.as_secs(),
                            self.max_retries
                        ));
                    }
                }
            }
        }

        match last_error {
            Some(e) => bail!(
                "All {} LLM provider(s) failed. Providers: [{}]. Last error: {}",
                self.clients.len(),
                self.clients
                    .iter()
                    .map(|c| c.model())
                    .collect::<Vec<_>>()
                    .join(", "),
                e
            ),
            None => bail!("No LLM providers configured"),
        }
    }

    fn model(&self) -> &str {
        self.clients
            .get(self.current_idx)
            .map(|c| c.model())
            .unwrap_or("unknown")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_proxy_is_configured() {
        // When PROXY_URL is not set
        std::env::remove_var("PROXY_URL");
        std::env::remove_var("ANTHROPIC_BASE_URL");
        assert!(!proxy_is_configured());
    }

    #[test]
    fn test_resolve_provider_for_model() {
        std::env::set_var(
            "MODEL_PROVIDER_MAP",
            "glm=openai,gpt=openai,claude=anthropic",
        );

        assert_eq!(
            resolve_provider_for_model("glm-5"),
            Some("openai".to_string())
        );
        assert_eq!(
            resolve_provider_for_model("gpt-4o"),
            Some("openai".to_string())
        );
        assert_eq!(
            resolve_provider_for_model("claude-3"),
            Some("anthropic".to_string())
        );
        assert_eq!(resolve_provider_for_model("unknown-model"), None);

        std::env::remove_var("MODEL_PROVIDER_MAP");
    }

    #[test]
    fn test_has_api_key_for_provider() {
        std::env::remove_var("ANTHROPIC_API_KEY");
        std::env::remove_var("OPENAI_API_KEY");
        std::env::remove_var("GEMINI_API_KEY");
        std::env::remove_var("FIREWORKS_API_KEY");

        assert!(!has_api_key_for_provider("anthropic"));
        assert!(!has_api_key_for_provider("openai"));
        assert!(!has_api_key_for_provider("gemini"));
        assert!(!has_api_key_for_provider("fireworks"));

        std::env::set_var("ANTHROPIC_API_KEY", "test-key");
        assert!(has_api_key_for_provider("anthropic"));
        std::env::remove_var("ANTHROPIC_API_KEY");

        std::env::set_var("FIREWORKS_API_KEY", "fw_test-key");
        assert!(has_api_key_for_provider("fireworks"));
        std::env::remove_var("FIREWORKS_API_KEY");
    }
}
