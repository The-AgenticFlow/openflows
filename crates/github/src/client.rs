// crates/github/src/client.rs
//
// McpGithubClient — uses modelcontextprotocol/server-github tool calls.
// This crate acts as a bridge between the Rust agents and the MCP tools.

use anyhow::{Context, Result};
use serde_json::{json, Value};
use tracing::debug;

pub struct McpGithubClient {
    pub mcp_command: String, // e.g. "mcp-cli" or "mcpcurl"
}

impl McpGithubClient {
    pub fn new(command: impl Into<String>) -> Self {
        Self {
            mcp_command: command.into(),
        }
    }

    /// Invoke an MCP GH tool by name with arguments.
    pub async fn call(&self, tool_name: &str, args: Value) -> Result<Value> {
        debug!(tool = tool_name, ?args, "Invoking GitHub MCP tool");

        // Format: mcp-cli call github-mcp-server <tool_name> --args <json>
        let output = tokio::process::Command::new(&self.mcp_command)
            .args(["call", "github-mcp-server", tool_name])
            .arg("--args")
            .arg(serde_json::to_string(&args)?)
            .output()
            .await
            .context("Failed to execute MCP CLI")?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            return Err(anyhow::anyhow!("MCP tool call failed: {}", stderr));
        }

        let stdout = String::from_utf8_lossy(&output.stdout);
        let res: Value =
            serde_json::from_str(&stdout).context("Failed to parse MCP output JSON")?;
        Ok(res)
    }

    // ── High Level Helpers ────────────────────────────────────────────────

    pub async fn list_pull_requests(&self, owner: &str, repo: &str) -> Result<Vec<Value>> {
        let res = self
            .call(
                "list_pull_requests",
                json!({
                    "owner": owner,
                    "repo": repo,
                    "state": "open"
                }),
            )
            .await?;

        Ok(res.as_array().cloned().unwrap_or_default())
    }

    pub async fn search_issues(&self, query: &str) -> Result<Vec<Value>> {
        let res = self
            .call(
                "search_issues",
                json!({
                    "query": query
                }),
            )
            .await?;

        Ok(res["items"].as_array().cloned().unwrap_or_default())
    }

    pub async fn push_files(
        &self,
        owner: &str,
        repo: &str,
        branch: &str,
        files: Vec<Value>,
        message: &str,
    ) -> Result<()> {
        self.call(
            "push_files",
            json!({
                "owner": owner,
                "repo": repo,
                "branch": branch,
                "files": files,
                "message": message
            }),
        )
        .await?;
        Ok(())
    }

    pub async fn create_pull_request(
        &self,
        owner: &str,
        repo: &str,
        title: &str,
        head: &str,
        base: &str,
    ) -> Result<u64> {
        let res = self
            .call(
                "create_pull_request",
                json!({
                    "owner": owner,
                    "repo": repo,
                    "title": title,
                    "head": head,
                    "base": base
                }),
            )
            .await?;

        res["number"]
            .as_u64()
            .context("Missing PR number in response")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    #[ignore] // Requires mcp-cli in env
    async fn test_mcp_call_format() {
        let _client = McpGithubClient::new("mcp-cli");
        // This is a dry run of the command construction
        // We'd need a mock for Command if we wanted a pure unit test
    }
}
