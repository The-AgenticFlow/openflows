pub mod anthropic;
pub mod fallback;
pub mod fireworks;
pub mod gemini;
pub mod mcp;
pub mod openai;
pub mod runner;
pub mod truncate;
pub mod types;

pub use anthropic::AnthropicClient;
pub use fallback::FallbackClient;
pub use fireworks::FireworksClient;
pub use gemini::GeminiClient;
pub use mcp::McpSession;
pub use openai::OpenAiClient;
pub use runner::AgentRunner;
pub use types::{
    AgentDecision, AgentPersona, LlmClient, LlmResponse, Message, ToolResult, ToolSchema,
};

/// Strip a provider prefix (e.g. "anthropic/", "openai/", "fireworks/", "gemini/", "groq/")
/// from a model identifier.  API clients and CLI backends expect bare model names
/// (e.g. "claude-haiku-4-5-20251001"), but the registry may store them with a
/// routing prefix (e.g. "anthropic/claude-haiku-4-5-20251001").
pub fn strip_provider_prefix(model: &str) -> &str {
    model
        .strip_prefix("anthropic/")
        .or_else(|| model.strip_prefix("openai/"))
        .or_else(|| model.strip_prefix("fireworks/"))
        .or_else(|| model.strip_prefix("gemini/"))
        .or_else(|| model.strip_prefix("groq/"))
        .unwrap_or(model)
}
