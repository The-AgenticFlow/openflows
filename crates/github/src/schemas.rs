// crates/github/src/schemas.rs
//
// Command helper to spawn the GitHub MCP server.
// All tool calls are made through the GitHub REST/MCP interface.

/// Returns the default command to spawn the GitHub MCP server via Docker.
/// The GITHUB_PERSONAL_ACCESS_TOKEN env var must be set in the environment.
pub fn github_mcp_cmd() -> Vec<&'static str> {
    vec![
        "docker",
        "run",
        "-i",
        "--rm",
        "-e",
        "GITHUB_PERSONAL_ACCESS_TOKEN",
        "ghcr.io/github/github-mcp-server",
    ]
}
