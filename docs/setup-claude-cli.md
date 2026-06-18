# Setting Up CLI Backends for AgentFlow

AgentFlow supports two CLI backends: **Claude Code** (Anthropic) and **Codex** (OpenAI). This guide covers installing and configuring both.

## Problem

The FORGE process requires a CLI backend to be installed and accessible. If you see errors like:

```
WARN pair_harness::pair: FORGE exited unexpectedly - synthesizing handoff
WARN pair_harness::reset: No WORKLOG.md to synthesize handoff from
```

repeatedly in the logs, followed by "Maximum context resets exceeded", it means the CLI binary cannot be found or executed.

---

## Claude Code CLI

### 1. Install Claude CLI

Visit https://claude.ai/download or follow the installation instructions for your operating system.

### 2. Find the Claude CLI Path

After installation, find where the `claude` binary is located:

**On Linux/macOS:**
```bash
which claude
# Example output: /usr/local/bin/claude
# or: /home/user/.local/bin/claude
```

**On Windows:**
```powershell
where claude
# Example output: C:\Program Files\Claude\claude.exe
```

### 3. Configure the Path

Edit your `.env` file and set the `CLAUDE_PATH` variable:

```env
# For Linux/macOS:
CLAUDE_PATH=/usr/local/bin/claude

# Or if it's in your PATH and accessible:
CLAUDE_PATH=claude

# For Windows:
CLAUDE_PATH=C:\Program Files\Claude\claude.exe
```

### 4. Verify Installation

Test that the Claude CLI is accessible:

```bash
# On Linux/macOS:
$CLAUDE_PATH --version

# On Windows:
claude --version
```

You should see version information printed. If you get "command not found" or similar errors, the path is incorrect.

### 5. Alternative: Add to PATH

Instead of setting `CLAUDE_PATH`, you can add the Claude CLI directory to your system PATH:

**Linux/macOS (add to ~/.bashrc or ~/.zshrc):**
```bash
export PATH="/path/to/claude/directory:$PATH"
```

**Windows (System Environment Variables):**
1. Search for "Environment Variables" in Start menu
2. Edit the `Path` variable
3. Add the directory containing `claude.exe`

Then set `CLAUDE_PATH=claude` in your `.env` file.

## Codex CLI

### 1. Install Codex CLI

```bash
npm install -g @openai/codex
```

### 2. Find the Codex CLI Path

```bash
which codex
# Example output: /usr/local/bin/codex
```

### 3. Configure the Path

Edit your `.env` file and set the `CODEX_PATH` variable:

```env
# If codex is in your PATH:
CODEX_PATH=codex

# Or specify the full path:
CODEX_PATH=/usr/local/bin/codex
```

### 4. Set Your OpenAI API Key

Codex requires an OpenAI-compatible API. Set one of:

```env
# OpenAI direct
OPENAI_API_KEY=sk-proj-xxxxx

# Fireworks (recommended for cost savings)
FIREWORKS_API_KEY=fw-xxxxx
OPENAI_BASE_URL=https://api.fireworks.ai/inference/v1
DEFAULT_CLI=codex
```

See [docs/cli-backend-configuration.md](cli-backend-configuration.md) for provider compatibility details.

### 5. Verify Installation

```bash
codex --version
```

---

## Environment Variables Summary

Required in `.env` (choose one backend):

```env
# ── Claude Backend ──────────────────────────────────
CLAUDE_PATH=/usr/local/bin/claude
ANTHROPIC_API_KEY=your_api_key_here
DEFAULT_CLI=claude

# ── Codex Backend ────────────────────────────────────
CODEX_PATH=/usr/local/bin/codex
OPENAI_API_KEY=your_api_key_here
DEFAULT_CLI=codex

# ── Required for both ────────────────────────────────
GITHUB_PERSONAL_ACCESS_TOKEN=your_github_token_here
GITHUB_REPOSITORY=owner/repo
```

## Troubleshooting

### Error: "Failed to spawn FORGE process"

- **Cause:** CLI binary not found
- **Fix:** Double-check `CLAUDE_PATH` or `CODEX_PATH` in your `.env` file

### Error: "Permission denied"

- **Cause:** Binary doesn't have execute permissions
- **Fix (Linux/macOS):**
  ```bash
  chmod +x /path/to/claude    # or /path/to/codex
  ```

### FORGE Process Exits Immediately

- **Cause:** Usually missing/incorrect CLI path, or missing API key
- **Fix:**
  1. Verify `CLAUDE_PATH`/`CODEX_PATH` points to a valid binary
  2. Ensure `ANTHROPIC_API_KEY` (Claude) or `OPENAI_API_KEY` (Codex) is set in `.env`
  3. Check the binary works: `$CLAUDE_PATH --version` or `$CODEX_PATH --version`

### Codex: "Network access restricted"

- **Cause:** Codex sandbox blocks outbound traffic by default
- **Fix:** In `.codex/config.toml`, enable network access:
  ```toml
  [sandbox_workspace_write]
  network_access = true
  ```
  Or configure domain allowlisting via the setup wizard (`openflows-setup`).

## Environment Variables Summary

Required in `.env`:
```env
# Claude CLI binary path
CLAUDE_PATH=/usr/local/bin/claude

# Anthropic API key (required for Claude)
ANTHROPIC_API_KEY=your_api_key_here

# GitHub token for MCP
GITHUB_PERSONAL_ACCESS_TOKEN=your_github_token_here
```

## Next Steps

After configuring your CLI backend:

1. Restart your AgentFlow application
2. Monitor the logs for successful FORGE process spawning
3. You should see: `INFO pair_harness::process: FORGE process spawned pair="forge-1" pid=Some(xxxxx)`
4. The process should stay running and create a `WORKLOG.md` file

For switching between backends, see [docs/cli-backend-configuration.md](cli-backend-configuration.md).
