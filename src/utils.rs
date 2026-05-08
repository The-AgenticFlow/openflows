use anyhow::Result;

pub async fn call_llm(prompt: &str) -> Result<String> {
    // TODO: Replace with real LLM call (e.g., OpenAI via reqwest)
    // Placeholder echoes a canned response
    Ok(format!("[LLM answer to]: {}", prompt))
}
