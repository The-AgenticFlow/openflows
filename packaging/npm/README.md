# @the-agenticflow/openflows npm Package

## Installation

```bash
# Install globally
npm install -g @the-agenticflow/openflows

# Or use with npx (no install needed)
npx @the-agenticflow/openflows setup
```

## What Gets Installed

The post-install script (`scripts/install.js`) downloads platform-specific binaries from GitHub Releases:

1. **Detects Platform**
   - macOS: `x86_64-apple-darwin` or `aarch64-apple-darwin`
   - Linux: `x86_64-unknown-linux-gnu/musl` or `aarch64-unknown-linux-gnu`

2. **Downloads Binary**
   - Fetches latest release from GitHub API
   - Falls back to `v0.1.3` if API unavailable
   - Downloads tarball containing:
     - `agentflow` - Main orchestration binary
     - `agentflow-setup` - Interactive TUI setup wizard
     - `agentflow-dashboard` - Real-time monitoring TUI
     - `agentflow-doctor` - System diagnostics tool
     - `orchestration/` - Agent configurations
     - `LICENSE` - MIT License

3. **Extracts to Package**
   - Extracts to `bin/` directory
   - Renames binaries to `*-bin` for Node.js wrappers
   - Sets executable permissions

## Commands Available

```bash
openflows              # Start orchestration
openflows-setup        # Run setup wizard
openflows-dashboard    # Launch monitoring TUI
openflows-doctor       # Run diagnostics
```

## Troubleshooting

### Permission Denied (sudo install)

**Problem**: `EACCES: permission denied, open '/tmp/openflows-...'`

**Solution**: Fixed in v0.1.3 - install script now uses package-local `.tmp` directory instead of `/tmp`.

**Workaround for older versions**:
```bash
# Install without sudo
mkdir -p ~/.npm-global
npm config set prefix '~/.npm-global'
echo 'export PATH=~/.npm-global/bin:$PATH' >> ~/.bashrc
source ~/.bashrc
npm install -g @the-agenticflow/openflows
```

### Network Issues (DNS/API failures)

**Problem**: `getaddrinfo EAI_AGAIN api.github.com` or `undefined` in filename

**Solution**: Fixed in v0.1.3 - script now:
- Has timeout handling for GitHub API
- Falls back to known version `v0.1.3` if API fails
- Validates API response before using

**Manual fix**:
```bash
# Build from source instead
git clone https://github.com/The-AgenticFlow/OpenFlows.git
cd OpenFlows
cargo build --release --bin agentflow-setup
sudo cp target/release/agentflow-setup /usr/local/bin/
```

### Binary Not Found

**Problem**: Command not found after install

**Solution**: Ensure `npm bin -g` is in your PATH
```bash
# Check where global bins are installed
npm bin -g

# Add to PATH if needed
echo 'export PATH="$(npm bin -g):$PATH"' >> ~/.bashrc
source ~/.bashrc
```

## Publishing New Versions

When a new GitHub release is created:

1. **Update package.json version**:
   ```bash
   cd packaging/npm
   # Edit package.json: "version": "0.1.4"
   ```

2. **Update fallback version in install.js**:
   ```javascript
   // Line 126
   tag = 'v0.1.4'; // Update fallback
   ```

3. **Test locally**:
   ```bash
   npm pack
   npm install -g the-agenticflow-openflows-0.1.4.tgz
   openflows-setup --help
   ```

4. **Publish to npm**:
   ```bash
   npm publish --access public
   ```

## Files Included in npm Package

```
@the-agenticflow/openflows/
├── package.json        # Package metadata
├── README.md           # This file
├── bin/
│   ├── openflows.js           # Node.js wrapper
│   ├── openflows-setup.js     # Node.js wrapper
│   ├── openflows-dashboard.js # Node.js wrapper
│   └── openflows-doctor.js    # Node.js wrapper
└── scripts/
    └── install.js      # Post-install binary downloader
```

## Technical Details

### Platform Detection Logic

```javascript
// OS detection
darwin  → apple-darwin
linux   → unknown-linux-gnu OR unknown-linux-musl

// Architecture detection
x64   → x86_64
arm64 → aarch64

// Musl detection (Linux only)
ldd --version | grep -q musl → use musl variant
```

### Fallback Chain

1. Try GitHub API for latest release
2. If API fails → use hardcoded `v0.1.3`
3. Download `gnu` variant
4. If `gnu` fails on x86_64 Linux → try `musl` variant
5. Extract and install

### Binary Wrappers

Each command (`openflows`, `openflows-setup`, etc.) is a Node.js wrapper that:
- Calls the downloaded binary (`*-bin`)
- Passes through all arguments
- Inherits stdio for interactive TUI
- Exits with binary's exit code

## License

MIT - see LICENSE file in downloaded tarball or GitHub repository.
