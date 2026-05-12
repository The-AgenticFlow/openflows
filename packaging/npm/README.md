# @the-agenticflow/openflows

**Autonomous AI Development Team** — Install and run with zero configuration knowledge required.

## Quick Start

```bash
# 1. Install globally
npm install -g @the-agenticflow/openflows

# 2. Run setup wizard (configures API keys)
openflows-setup

# 3. Start the autonomous team
openflows
```

That's it! The package handles everything automatically.

## How It Works

### Zero-Configuration Philosophy

You don't need to know about proxies, MCP servers, or backend routing. The package:

1. **Installs all dependencies** - Including `anthropic-proxy` for LLM API translation
2. **Auto-detects your setup** - Fireworks key? Proxy starts automatically. Anthropic key? Direct mode.
3. **Manages the proxy lifecycle** - Starts/stops the built-in proxy as needed
4. **Provides helpful errors** - Clear messages when something needs attention

### Configuration Modes

| Mode | What You Need | Proxy Required? |
|------|---------------|-----------------|
| **Fireworks AI** | `FIREWORKS_API_KEY` + `PROXY_TARGET_MODEL` | Auto-started (built-in) |
| **Anthropic Direct** | `ANTHROPIC_API_KEY` | No |
| **Custom Gateway** | `GATEWAY_URL` + `GATEWAY_API_KEY` | Auto-started |

### Fireworks AI Setup (Recommended)

Fireworks AI provides cost-effective OpenAI-compatible endpoints. Since Claude Code CLI speaks the Anthropic Messages API, the package includes a built-in protocol translator that automatically starts.

**Via the setup wizard:**

```bash
openflows-setup
```

When you select **Fireworks AI** as your provider, the wizard will:
1. Ask for your `FIREWORKS_API_KEY`
2. Show the proxy configuration screen with these fields:
   - **Proxy URL** — pre-filled as `http://localhost:8765/v1`
   - **Proxy API Key** — your Fireworks key (auto-filled)
   - **Target Model (PROXY_TARGET_MODEL)** — e.g., `accounts/fireworks/models/glm-5`
   - **Gateway URL** — pre-filled as `https://api.fireworks.ai/inference/v1/`
   - **Gateway API Key** — your Fireworks key (auto-filled)
3. Complete the remaining steps (agents, GitHub, repository)

**What `PROXY_TARGET_MODEL` does:**

Instead of manually mapping each Claude model name, set this once. The local proxy:
1. Strips ANSI escape codes from incoming model names
2. Detects Claude patterns (`claude-*`, `opus`, `sonnet`, `haiku`)
3. Routes all of them to your specified target model

```env
PROXY_TARGET_MODEL=accounts/fireworks/models/glm-5
```

**Manual setup (without wizard):**

```bash
# Create .env in your project directory
cat > .env << 'EOF'
FIREWORKS_API_KEY=fw_your_key
PORT=8765
PROXY_URL=http://localhost:8765/v1
PROXY_API_KEY=fw_your_key
GATEWAY_URL=https://api.fireworks.ai/inference/v1/
GATEWAY_API_KEY=fw_your_key
PROXY_TARGET_MODEL=accounts/fireworks/models/glm-5
GITHUB_PERSONAL_ACCESS_TOKEN=ghp_your_token
GITHUB_REPOSITORY=owner/repo
EOF
```

### Commands

```bash
openflows              # Start orchestration
openflows-setup        # Interactive setup wizard (TUI)
openflows-dashboard    # Real-time monitoring (TUI)
openflows-doctor       # System diagnostics
openflows --help       # Show help
openflows --version    # Show version
```

## What Gets Installed

The post-install script downloads platform-specific binaries from GitHub Releases:

| Binary | Purpose |
|--------|---------|
| `agentflow` | Main orchestration engine |
| `agentflow-setup` | Interactive TUI setup wizard |
| `agentflow-dashboard` | Real-time monitoring TUI |
| `agentflow-doctor` | System diagnostics tool |
| `anthropic-proxy` | Built-in Anthropic-to-OpenAI protocol translator (auto-starts for Fireworks) |

Additionally:
- **mcp-proxy** is installed via npm for GitHub MCP connectivity
- **.env.example** is included for reference

## Environment Configuration

Create a `.env` file in your project directory:

```bash
# Required
GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxxxx
GITHUB_REPOSITORY=owner/repo-name

# Choose one API provider
FIREWORKS_API_KEY=fw_xxxxx        # Recommended — proxy auto-starts
PROXY_TARGET_MODEL=accounts/fireworks/models/glm-5  # Target model for Fireworks
# ANTHROPIC_API_KEY=sk-ant-xxxx   # Direct mode (no proxy needed)
# GATEWAY_URL=...                 # Custom gateway (proxy auto-starts)
# GATEWAY_API_KEY=...             # For custom gateways
```

Run `openflows-setup` for a guided configuration experience.

## Advanced Options

### Disable Auto-Proxy

```bash
openflows --no-proxy
```

### Proxy-Only Mode (Testing)

```bash
openflows --proxy-only
# Starts only the built-in proxy on port 8765
```

### Custom Proxy Port

```bash
PROXY_PORT=9000 openflows
```

### Docker MCP (Alternative)

If you prefer Docker for GitHub MCP:

```bash
export GITHUB_MCP_TYPE=docker
openflows
```

## Troubleshooting

### Fireworks Setup — Missing PROXY_TARGET_MODEL

If you selected Fireworks AI during setup but didn't see the model configuration screen, ensure you're using version **0.1.15** or later. Earlier versions only showed proxy config in Advanced mode.

```bash
openflows --version
# Should show 0.1.15 or later
```

If you're on an older version:
```bash
npm update -g @the-agenticflow/openflows
```

### `mcp-proxy` Installation Issues

The post-install script attempts to install `mcp-proxy` (Python tool from PyPI) automatically. If you see errors:

1. **Install manually:**
   ```bash
   # Recommended (fastest)
   uv tool install mcp-proxy
   
   # Alternative
   pipx install mcp-proxy
   ```

2. **Use Docker mode instead:**
   ```bash
   export GITHUB_MCP_TYPE=docker
   openflows
   ```

The `mcp-proxy` tool bridges stdio to HTTP MCP servers like GitHub Copilot's MCP endpoint.

### Permission Denied

Ensure you're not using `sudo` for npm install:

```bash
# Configure npm to use user-writable directory
mkdir -p ~/.npm-global
npm config set prefix '~/.npm-global'
echo 'export PATH=~/.npm-global/bin:$PATH' >> ~/.bashrc
source ~/.bashrc

# Now install without sudo
npm install -g @the-agenticflow/openflows
```

### Network Issues

If GitHub API is unreachable:

1. The installer falls back to a known version
2. Or build from source:
   ```bash
   git clone https://github.com/The-AgenticFlow/AgentFlow.git
   cd AgentFlow
   cargo build --release
   ```

### GitHub 401 Unauthorized

Ensure your `GITHUB_PERSONAL_ACCESS_TOKEN` is valid with these scopes:
- `repo` (full repository access)
- `read:user`
- `read:org`

## Platform Support

| OS | Architecture | Status |
|----|--------------|--------|
| macOS | x86_64 (Intel) | ✅ |
| macOS | aarch64 (M1/M2) | ✅ |
| Linux | x86_64 (glibc) | ✅ |
| Linux | x86_64 (musl) | ✅ |
| Linux | aarch64 | ✅ |

## Development

### Testing Locally

```bash
cd packaging/npm
npm pack
npm install -g the-agenticflow-openflows-0.1.4.tgz
openflows-setup --help
```

### Publishing

1. Update version in `package.json`
2. Update fallback version in `scripts/install.js`
3. Test locally with `npm pack`
4. Publish: `npm publish --access public`

## Files Included

```
@the-agenticflow/openflows/
├── package.json        # Package metadata
├── README.md           # This file
├── .env.example        # Configuration template
├── bin/
│   ├── openflows.js           # Main wrapper (handles proxy)
│   ├── openflows-setup.js     # Setup wizard wrapper
│   ├── openflows-dashboard.js # Dashboard wrapper
│   └── openflows-doctor.js    # Doctor wrapper
└── scripts/
    └── install.js      # Post-install script
```

## License

MIT - See [LICENSE](https://github.com/The-AgenticFlow/AgentFlow/blob/main/LICENSE)
