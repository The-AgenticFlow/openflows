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

1. **Installs all dependencies** - Including `mcp-proxy` for GitHub connectivity
2. **Auto-detects your setup** - Fireworks key? Direct mode. Gateway? Proxy starts automatically
3. **Manages the proxy lifecycle** - Starts/stops the built-in proxy as needed
4. **Provides helpful errors** - Clear messages when something needs attention

### Configuration Modes

| Mode | What You Need | Proxy Required? |
|------|---------------|-----------------|
| **Fireworks Direct** | `FIREWORKS_API_KEY` | No |
| **Anthropic Direct** | `ANTHROPIC_API_KEY` | No |
| **Custom Gateway** | `GATEWAY_URL` + `GATEWAY_API_KEY` | Auto-started |

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
| `anthropic-proxy` | Built-in Anthropic-to-OpenAI proxy |

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
FIREWORKS_API_KEY=fw_xxxxx        # Recommended - direct mode
# ANTHROPIC_API_KEY=sk-ant-xxxx   # Direct mode
# GATEWAY_URL=...                 # Proxy auto-starts
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
