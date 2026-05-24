# CLI Backend Configuration

AgentFlow supports multiple CLI backends for agent execution. This document explains how to configure and switch between different CLI backends.

## Supported Backends

| Backend | Description | Provider |
|---------|-------------|----------|
| `claude` | Claude Code CLI (default) | Anthropic |
| `codex` | Codex CLI | OpenAI |

## Configuration Hierarchy

The CLI backend is determined using a priority-based fallback chain:

```
┌─────────────────────────────────────────────────────────────┐
│  Priority 1: Agent-specific 'cli' field (HIGHEST)          │
│  ↓ If not set or empty                                       │
│  Priority 2: DEFAULT_CLI environment variable                │
│  ↓ If not set                                                │
│  Priority 3: default_cli field in registry.json             │
│  ↓ If not set                                                │
│  Priority 4: Hardcoded "claude" fallback (LOWEST)           │
└─────────────────────────────────────────────────────────────┘
```

## Configuration Methods

### Method 1: Per-Agent Configuration (Recommended)

Configure each agent individually in `orchestration/agent/registry.json`:

```json
{
  "default_cli": "claude",
  "team": [
    {
      "id": "nexus",
      "cli": "claude",
      "model": "claude-haiku-4-5-20251001",
      "active": true,
      "instances": 1
    },
    {
      "id": "forge",
      "cli": "codex",
      "model": "gpt-4o",
      "active": true,
      "instances": 2
    },
    {
      "id": "sentinel",
      "cli": "claude",
      "model": "claude-haiku-4-5-20251001",
      "active": true,
      "instances": 1
    },
    {
      "id": "vessel",
      "cli": "",
      "model": "claude-haiku-4-5-20251001",
      "active": true,
      "instances": 1
    }
  ]
}
```

**Explanation:**
- `nexus` → Uses Claude (explicit `"cli": "claude"`)
- `forge` → Uses Codex (explicit `"cli": "codex"`)
- `sentinel` → Uses Claude (explicit `"cli": "claude"`)
- `vessel` → Uses default (empty `cli` → falls back to `default_cli: "claude"`)

### Method 2: Environment Variable Override

Set the `DEFAULT_CLI` environment variable to override the default for all agents without an explicit `cli` field:

```bash
# In .env file
DEFAULT_CLI=codex
```

This is useful for:
- **Testing**: Switch all agents to Codex without modifying registry.json
- **Environment-specific configurations**: Use Claude in production, Codex in development
- **Quick switching**: Change backend without code changes

### Method 3: Registry Default

Set the `default_cli` field in `orchestration/agent/registry.json`:

```json
{
  "default_cli": "claude",
  "team": [...]
}
```

This applies to all agents that don't have an explicit `cli` field.

## Environment Variables

| Variable | Description | Default |
|----------|-------------|---------|
| `DEFAULT_CLI` | Override default CLI backend for all agents | (not set) |
| `CLAUDE_PATH` | Path to Claude CLI binary | `"claude"` |
| `CODEX_PATH` | Path to Codex CLI binary | `"codex"` |

## Example Configurations

### All Agents Use Codex

```bash
# .env
DEFAULT_CLI=codex
CODEX_PATH=codex
OPENAI_API_KEY=your_openai_api_key
OPENAI_BASE_URL=https://api.fireworks.ai/inference/v1
```

```json
// registry.json - all agents will use Codex
{
  "default_cli": "claude",  // Overridden by DEFAULT_CLI=codex
  "team": [
    { "id": "nexus", "cli": "", ... },    // Uses Codex (via DEFAULT_CLI)
    { "id": "forge", "cli": "", ... }     // Uses Codex (via DEFAULT_CLI)
  ]
}
```

### Mixed Backends (Per-Agent Override)

```json
{
  "default_cli": "claude",
  "team": [
    { "id": "nexus", "cli": "claude", ... },   // Uses Claude (explicit)
    { "id": "forge", "cli": "codex", ... },    // Uses Codex (explicit)
    { "id": "sentinel", "cli": "", ... }        // Uses Claude (via default_cli)
  ]
}
```

### Development vs Production

```bash
# .env.development
DEFAULT_CLI=codex  # Use Codex in development

# .env.production
DEFAULT_CLI=claude  # Use Claude in production
```

## Installation

### Claude Code CLI

```bash
# Install Claude Code CLI
npm install -g @anthropic-ai/claude-code

# Verify installation
claude --version

# Set path (optional, if not in PATH)
CLAUDE_PATH=/path/to/claude
```

### Codex CLI

```bash
# Install Codex CLI
npm install -g @openai/codex

# Verify installation
codex --version

# Set path (optional, if not in PATH)
CODEX_PATH=/path/to/codex
```

## How It Works

### Code Flow

1. **Registry Loading**: The system loads `orchestration/agent/registry.json`
2. **CLI Backend Resolution**: For each agent, the system resolves the CLI backend using the priority chain
3. **Pair Configuration**: The resolved backend is stored in `PairConfig`
4. **Process Spawning**: When spawning a CLI process, the system constructs the appropriate command

### CLI Commands

| Backend | Command |
|---------|---------|
| Claude | `claude --dangerously-skip-permissions -p <prompt> --output-format stream-json` |
| Codex | `codex --approval-mode full-auto -q "<prompt>"` |

### Key Files

| File | Purpose |
|------|---------|
| `crates/config/src/registry.rs` | `CliBackend` enum, `resolve_cli_backend()` method |
| `crates/pair-harness/src/types.rs` | `PairConfig` with `cli_backend` field |
| `crates/pair-harness/src/process.rs` | CLI command construction per backend |
| `crates/agent-forge/src/lib.rs` | Resolves CLI backend when creating pairs |

## Troubleshooting

### CLI Not Found

```
Error: Failed to spawn CLI process: No such file or directory
```

**Solution**: Ensure the CLI is installed and in your PATH, or set the explicit path:

```bash
CLAUDE_PATH=/usr/local/bin/claude
CODEX_PATH=/usr/local/bin/codex
```

### Wrong Backend Used

Check the priority chain:
1. Agent-specific `cli` field in registry.json
2. `DEFAULT_CLI` environment variable
3. `default_cli` in registry.json
4. Hardcoded default (`claude`)

### Codex Approval Issues

Codex approval mode is hard-coded as `full-auto` in AgentFlow's process spawning logic. If you need to change this, modify `BackendConfig::codex()` in `crates/pair-harness/src/process.rs`.

## Provider Compatibility

### Codex CLI (OpenAI-Compatible — No Proxy Needed)

| Provider | `OPENAI_BASE_URL` | Notes |
|----------|-------------------|-------|
| Fireworks | `https://api.fireworks.ai/inference/v1` | Set `OPENAI_API_KEY` to your Fireworks key |
| OpenAI | (default) | Set `OPENAI_API_KEY` to your OpenAI key |
| DeepSeek | `https://api.deepseek.com/v1` | Set `OPENAI_API_KEY` to your DeepSeek key |
| Together | `https://api.together.xyz/v1` | Set `OPENAI_API_KEY` to your Together key |
| Groq | `https://api.groq.com/openai/v1` | Set `OPENAI_API_KEY` to your Groq key |

Codex uses the OpenAI Chat Completions format, which all these providers support natively.

### Claude Code CLI (Anthropic Format — Proxy Required for Third-Party Providers)

| Provider | Proxy Required? | How |
|----------|----------------|-----|
| Anthropic (direct key) | No | Just set `ANTHROPIC_API_KEY` |
| Any OpenAI-compatible provider | **Yes** | Run `anthropic-mock` proxy to translate formats |

Claude Code uses the Anthropic Messages format. Only Anthropic's own API supports this natively.
For any other provider, you MUST run the `anthropic-mock` proxy:

```bash
# Terminal 1: Start the proxy
cargo run -p anthropic-mock

# Terminal 2: Run orchestration
cargo run --bin agentflow
```

The proxy translates:
- **Inbound**: Claude Code sends Anthropic `/v1/messages` requests to `localhost:8765`
- **Outbound**: Proxy translates to OpenAI `/v1/chat/completions` and forwards to `GATEWAY_URL`
- **Response**: Proxy translates OpenAI response back to Anthropic format

## Related Documentation

- [CONTRIBUTING.md](../CONTRIBUTING.md) - Development setup and guidelines
- [.env.example](../.env.example) - Environment variable reference
- [orchestration/agent/registry.json](../orchestration/agent/registry.json) - Agent configuration