// crates/agent-client/src/mcp.rs
//
// McpSession — spawns a GitHub MCP server as a stdio subprocess and
// communicates via JSON-RPC 2.0.
//
// Protocol flow:
//   1. Spawn process (docker, npx, or direct node)
//   2. Wait for readiness signal on stderr (e.g. "running on stdio")
//   3. Send `initialize` request
//   4. Receive `initialize` response
//   5. Send `initialized` notification
//   6. Ready: call list_tools / call_tool freely

use anyhow::{bail, Context, Result};
use serde_json::{json, Value};
use std::time::Duration;
use tokio::{
    io::{AsyncBufReadExt, AsyncWriteExt, BufReader},
    process::{Child, ChildStderr, ChildStdin, ChildStdout, Command},
};
use tracing::{debug, info, warn};

/// Command to spawn the local GitHub MCP server via Docker.
pub const DOCKER_MCP_CMD: &[&str] = &[
    "docker",
    "run",
    "-i",
    "--rm",
    "-e",
    "GITHUB_PERSONAL_ACCESS_TOKEN",
    "ghcr.io/github/github-mcp-server",
];

/// Default timeout for individual MCP JSON-RPC requests (seconds).
/// The GitHub MCP server disconnects its session after ~10s of inactivity,
/// so this must be shorter than that to catch unresponsive servers.
const DEFAULT_REQUEST_TIMEOUT_SECS: u64 = 30;

/// Default timeout for MCP server startup readiness (seconds).
/// When launching via `npx`, the server can take significant time to resolve
/// the package, download it (first run), and start. This timeout controls how
/// long we wait for the server's readiness signal on stderr before proceeding.
const DEFAULT_STARTUP_TIMEOUT_SECS: u64 = 60;

/// Substrings that indicate an MCP server has finished starting up and is
/// ready to receive JSON-RPC requests on stdin.
const READY_PATTERNS: &[&str] = &["running on stdio", "server started", "mcp server ready"];

pub struct McpSession {
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
    /// Stderr reader kept alive so the server's stderr pipe doesn't fill up
    /// and block the process.  Not actively read after startup readiness
    /// detection completes.
    #[allow(dead_code)]
    stderr: Option<BufReader<ChildStderr>>,
    next_id: u64,
    request_timeout: Duration,
}

impl McpSession {
    /// Spawn the MCP server and perform the JSON-RPC initialization handshake.
    pub async fn connect(cmd: &[&str]) -> Result<Self> {
        Self::connect_with_timeout(cmd, Self::default_timeout()).await
    }

    /// Spawn the MCP server with a custom request timeout.
    pub async fn connect_with_timeout(cmd: &[&str], timeout: Duration) -> Result<Self> {
        info!(cmd = ?cmd, timeout_secs = timeout.as_secs(), "Spawning GitHub MCP server");

        let mut child = Command::new(cmd[0])
            .args(&cmd[1..])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped()) // capture stderr for readiness detection
            .spawn()
            .context("Failed to spawn GitHub MCP server")?;

        let stdin = child.stdin.take().context("Failed to open MCP stdin")?;
        let stdout = BufReader::new(child.stdout.take().context("Failed to open MCP stdout")?);
        let stderr = BufReader::new(child.stderr.take().context("Failed to open MCP stderr")?);

        let mut session = Self {
            child,
            stdin,
            stdout,
            stderr: Some(stderr),
            next_id: 1,
            request_timeout: timeout,
        };

        // Wait for the server to signal readiness on stderr before sending
        // any JSON-RPC messages.  This is critical when launching via `npx`,
        // which has significant startup overhead (package resolution, download,
        // Node.js bootstrap) and does not reliably forward stdio until the
        // actual server process is running.
        session.wait_for_ready().await?;

        session.initialize().await?;
        Ok(session)
    }

    /// Resolve the request timeout from the `MCP_REQUEST_TIMEOUT_SECS` env var,
    /// falling back to `DEFAULT_REQUEST_TIMEOUT_SECS`.
    fn default_timeout() -> Duration {
        let secs = std::env::var("MCP_REQUEST_TIMEOUT_SECS")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(DEFAULT_REQUEST_TIMEOUT_SECS);
        Duration::from_secs(secs)
    }

    /// Convenience: connects to either the 'hosted' or 'docker' MCP implementation.
    /// Hierarchy:
    ///   1. GITHUB_MCP_CMD (Full command override)
    ///   2. GITHUB_MCP_TYPE (Either 'docker' or 'hosted', defaults to 'hosted')
    pub async fn connect_default() -> Result<Self> {
        // 1. Check for full command override
        if let Ok(cmd_str) = std::env::var("GITHUB_MCP_CMD") {
            let cmd: Vec<&str> = cmd_str.split_whitespace().collect();
            if !cmd.is_empty() {
                return Self::connect(&cmd).await;
            }
        }

        // 2. Check for type selection
        let mcp_type = std::env::var("GITHUB_MCP_TYPE").unwrap_or_else(|_| "hosted".to_string());
        match mcp_type.as_str() {
            "docker" => Self::connect(DOCKER_MCP_CMD).await,
            _ => Self::connect_hosted().await,
        }
    }

    /// Spawn the hosted MCP bridge (mcp-proxy), injecting the required env vars
    /// and arguments for the GitHub Copilot MCP endpoint.
    async fn connect_hosted() -> Result<Self> {
        let pat = std::env::var("GITHUB_PERSONAL_ACCESS_TOKEN")
            .context("GITHUB_PERSONAL_ACCESS_TOKEN must be set for hosted MCP")?;

        Self::connect_hosted_with_token(&pat).await
    }

    /// Spawn the GitHub MCP server with an explicit token.
    /// Respects GITHUB_MCP_TYPE env var: "docker" uses Docker, anything else uses hosted mcp-proxy.
    /// If GITHUB_MCP_CMD is set, uses that instead (for testing/mocking).
    pub async fn connect_hosted_with_token(pat: &str) -> Result<Self> {
        // 1. Check for full command override (for testing/mocking)
        if let Ok(cmd_str) = std::env::var("GITHUB_MCP_CMD") {
            let cmd: Vec<&str> = cmd_str.split_whitespace().collect();
            if !cmd.is_empty() {
                info!(cmd = ?cmd, "Using GITHUB_MCP_CMD override");
                // Set the token in env for the mock to potentially use
                std::env::set_var("GITHUB_PERSONAL_ACCESS_TOKEN", pat);
                return Self::connect(&cmd).await;
            }
        }

        let mcp_type = std::env::var("GITHUB_MCP_TYPE").unwrap_or_else(|_| "hosted".to_string());
        match mcp_type.as_str() {
            "docker" => {
                // Docker MCP doesn't need the PAT as an argument — it uses env var
                std::env::set_var("GITHUB_PERSONAL_ACCESS_TOKEN", pat);
                Self::connect(DOCKER_MCP_CMD).await
            }
            _ => {
                info!("Spawning hosted GitHub MCP server via mcp-proxy bridge");
                let mut child = Command::new("mcp-proxy")
                    .arg("convert")
                    .arg("https://api.githubcopilot.com/mcp/")
                    .arg("--auth")
                    .arg(format!("Bearer {}", pat))
                    .arg("--protocol")
                    .arg("stream")
                    .stdin(std::process::Stdio::piped())
                    .stdout(std::process::Stdio::piped())
                    .stderr(std::process::Stdio::piped())
                    .spawn()
                    .context("Failed to spawn hosted MCP npx bridge")?;

                let stdin = child.stdin.take().context("Failed to open MCP stdin")?;
                let stdout =
                    BufReader::new(child.stdout.take().context("Failed to open MCP stdout")?);
                let stderr =
                    BufReader::new(child.stderr.take().context("Failed to open MCP stderr")?);

                let mut session = Self {
                    child,
                    stdin,
                    stdout,
                    stderr: Some(stderr),
                    next_id: 1,
                    request_timeout: Self::default_timeout(),
                };

                // Wait for mcp-proxy to signal readiness before initializing.
                session.wait_for_ready().await?;

                session.initialize().await?;
                Ok(session)
            }
        }
    }

    // ── Private: Startup readiness ──────────────────────────────────────────

    /// Wait for the MCP server to signal readiness on stderr.
    ///
    /// When launched via `npx`, the MCP server process does not accept
    /// JSON-RPC input until the Node.js process has finished resolving and
    /// downloading the package.  The `@modelcontextprotocol/server-github`
    /// package prints "GitHub MCP Server running on stdio" to stderr once it
    /// is ready to receive requests.
    ///
    /// This method reads stderr lines until one of the `READY_PATTERNS` is
    /// detected.  If no pattern is found within the startup timeout, the
    /// method logs a warning and proceeds anyway — the server may not print
    /// a readiness signal, or this may be a mock/test server.
    async fn wait_for_ready(&mut self) -> Result<()> {
        let startup_timeout = Duration::from_secs(
            std::env::var("MCP_STARTUP_TIMEOUT_SECS")
                .ok()
                .and_then(|s| s.parse().ok())
                .unwrap_or(DEFAULT_STARTUP_TIMEOUT_SECS),
        );

        let stderr = match &mut self.stderr {
            Some(s) => s,
            None => {
                info!("No stderr reader — assuming MCP server is ready");
                return Ok(());
            }
        };

        let start = std::time::Instant::now();
        let mut line = String::new();

        info!("Waiting for MCP server readiness signal on stderr...");

        loop {
            if start.elapsed() > startup_timeout {
                warn!(
                    elapsed_secs = start.elapsed().as_secs(),
                    timeout_secs = startup_timeout.as_secs(),
                    "MCP server startup timeout exceeded — proceeding without readiness signal"
                );
                return Ok(());
            }

            line.clear();
            match tokio::time::timeout(Duration::from_secs(5), stderr.read_line(&mut line)).await {
                Ok(Ok(0)) => {
                    // EOF — server closed stderr
                    let status = self.child.try_wait();
                    match status {
                        Ok(Some(exit)) => {
                            bail!(
                                "MCP server exited during startup: {} — \
                                 the server may have crashed or the command is invalid.",
                                exit
                            );
                        }
                        Ok(None) => {
                            // Process still alive but closed stderr — some servers
                            // close stderr after startup.  Proceed with initialization.
                            info!("MCP server closed stderr during startup — proceeding with initialization");
                            return Ok(());
                        }
                        Err(e) => {
                            warn!(error = %e, "Could not check MCP server status after stderr EOF — proceeding");
                            return Ok(());
                        }
                    }
                }
                Ok(Ok(_bytes)) => {
                    let trimmed = line.trim();
                    if !trimmed.is_empty() {
                        info!(line = trimmed, "MCP server stderr");
                        let lower = trimmed.to_lowercase();
                        if READY_PATTERNS.iter().any(|p| lower.contains(p)) {
                            info!("MCP server ready (detected readiness signal on stderr)");
                            return Ok(());
                        }
                    }
                }
                Ok(Err(e)) => {
                    warn!(error = %e, "Error reading MCP server stderr — proceeding with initialization");
                    return Ok(());
                }
                Err(_) => {
                    // 5-second timeout on this individual read.
                    // Check if the server is still alive before continuing to wait.
                    if let Ok(Some(status)) = self.child.try_wait() {
                        bail!(
                            "MCP server exited during startup: {} — \
                             the server may have crashed or the startup command is invalid.",
                            status
                        );
                    }
                    // Server still alive but no stderr output yet — keep waiting
                }
            }
        }
    }

    // ── Private: JSON-RPC helpers ─────────────────────────────────────────

    fn next_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    async fn send_request(&mut self, method: &str, params: Value) -> Result<Value> {
        let id = self.next_id();
        let req = json!({
            "jsonrpc": "2.0",
            "id":      id,
            "method":  method,
            "params":  params,
        });

        let mut line = serde_json::to_string(&req)?;
        line.push('\n');
        debug!(method, id, "→ MCP request");
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;

        // Read response lines until we find our id, with a timeout.
        // The GitHub MCP Docker server disconnects its session after ~10s of
        // inactivity but the container stays alive — so read_line blocks
        // forever unless we impose our own deadline.
        let timeout = self.request_timeout;
        let result = tokio::time::timeout(timeout, async {
            loop {
                let mut buf = String::new();
                let bytes_read = self.stdout.read_line(&mut buf).await?;
                if bytes_read == 0 {
                    bail!(
                        "MCP server exited unexpectedly while waiting for response to '{}' (id={}). \
                         The subprocess may have crashed or timed out.",
                        method, id
                    );
                }
                let buf = buf.trim();
                if buf.is_empty() {
                    continue;
                }

                let resp: Value =
                    serde_json::from_str(buf).context("Failed to parse MCP JSON-RPC response")?;

                // Match on id (notifications won't have id)
                if resp["id"] == id {
                    debug!("← MCP response id={}", id);
                    if let Some(err) = resp.get("error") {
                        bail!("MCP error: {}", err);
                    }
                    return Ok(resp["result"].clone());
                }
                // Otherwise it's a notification — ignore for now
                debug!(notification = buf, "MCP notification (ignored)");
            }
        })
        .await;

        match result {
            Ok(inner) => inner,
            Err(_elapsed) => {
                // Timeout — check if the child process is still alive
                let status = self.child.try_wait();
                match status {
                    Ok(Some(exit_status)) => {
                        bail!(
                            "MCP request '{}' (id={}) timed out after {}s — server exited: {}",
                            method,
                            id,
                            timeout.as_secs(),
                            exit_status
                        );
                    }
                    Ok(None) => {
                        // Process alive but unresponsive — likely npx startup
                        // delay or session timeout
                        warn!(
                            method,
                            id,
                            timeout_secs = timeout.as_secs(),
                            "MCP server alive but unresponsive — killing subprocess"
                        );
                        let _ = self.child.kill().await;
                        bail!(
                            "MCP request '{}' (id={}) timed out after {}s — \
                             server is alive but not responding. \
                             Common causes: (1) MCP server not fully started yet \
                             (npx can take >30s to resolve the package); \
                             (2) session timeout after inactivity. \
                             Consider setting MCP_STARTUP_TIMEOUT_SECS to a higher \
                             value or using a direct 'node' command instead of 'npx'.",
                            method,
                            id,
                            timeout.as_secs()
                        );
                    }
                    Err(e) => {
                        bail!(
                            "MCP request '{}' (id={}) timed out after {}s — \
                             could not check process status: {}",
                            method,
                            id,
                            timeout.as_secs(),
                            e
                        );
                    }
                }
            }
        }
    }

    async fn send_notification(&mut self, method: &str, params: Value) -> Result<()> {
        let notif = json!({
            "jsonrpc": "2.0",
            "method":  method,
            "params":  params,
        });
        let mut line = serde_json::to_string(&notif)?;
        line.push('\n');
        self.stdin.write_all(line.as_bytes()).await?;
        self.stdin.flush().await?;
        Ok(())
    }

    // ── Initialization ────────────────────────────────────────────────────

    async fn initialize(&mut self) -> Result<()> {
        self.send_request(
            "initialize",
            json!({
                "protocolVersion": "2024-11-05",
                "capabilities":    {},
                "clientInfo": {
                    "name":    "agent-team",
                    "version": "0.1.0"
                }
            }),
        )
        .await?;

        // ACK the server's initialized notification
        self.send_notification("notifications/initialized", json!({}))
            .await?;
        info!("GitHub MCP server initialized");
        Ok(())
    }

    // ── Public API ────────────────────────────────────────────────────────

    /// Fetch all available tool schemas from the MCP server.
    /// These are forwarded verbatim as Anthropic tool definitions.
    pub async fn list_tools(&mut self) -> Result<Vec<crate::types::ToolSchema>> {
        let result = self.send_request("tools/list", json!({})).await?;

        let tools = result["tools"]
            .as_array()
            .context("MCP tools/list returned no 'tools' array")?;

        let schemas = tools
            .iter()
            .map(|t| crate::types::ToolSchema {
                name: t["name"].as_str().unwrap_or("").to_string(),
                description: t["description"].as_str().unwrap_or("").to_string(),
                input_schema: t["inputSchema"].clone(),
            })
            .collect();

        Ok(schemas)
    }

    /// Execute a named tool with the given arguments.
    pub async fn call_tool(&mut self, name: &str, args: Value) -> Result<crate::types::ToolResult> {
        debug!(tool = name, "Calling MCP tool");
        let result = self
            .send_request(
                "tools/call",
                json!({
                    "name":      name,
                    "arguments": args,
                }),
            )
            .await?;

        serde_json::from_value(result).context("Failed to parse MCP tool result")
    }
}

#[cfg(test)]
mod tests {
    /// Full integration test — requires Docker + GITHUB_PERSONAL_ACCESS_TOKEN.
    /// Run with: cargo test -p agent-client -- --ignored
    #[tokio::test]
    #[ignore]
    async fn test_mcp_list_tools() {
        let mut session = super::McpSession::connect_default().await.unwrap();
        let tools = session.list_tools().await.unwrap();
        assert!(
            !tools.is_empty(),
            "Expected at least one tool from GitHub MCP server"
        );
        println!(
            "Tools available: {}",
            tools
                .iter()
                .map(|t| t.name.as_str())
                .collect::<Vec<_>>()
                .join(", ")
        );
    }
}
