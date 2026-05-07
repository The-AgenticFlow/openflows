# License and Asset Distribution Flow

This document describes how the MIT license, documentation, and configuration assets are distributed across OpenFlows' packaging formats.

## Overview

OpenFlows uses a multi-format distribution strategy:
- **GitHub Releases**: Platform-specific tarballs with binaries + assets
- **npm Package**: `@the-agenticflow/openflows` with post-install binary downloader
- **Docker Images**: `ghcr.io/the-agenticflow/openflows` with embedded assets
- **crates.io**: Rust crate publication (library only, no assets)

## License

OpenFlows is released under the **MIT License**.

### License File Location
- **Repository Root**: `/LICENSE`
- **Copyright**: Christian Yemele (2026)
- **Permissions**: Free to use, modify, distribute, sublicense, and sell

### License Distribution Path

```
Repository Root (LICENSE)
    │
    ├─→ GitHub Release Tarball
    │   └─ Included in every platform archive
    │
    ├─→ npm Package
    │   └─ Declared in package.json "license": "MIT"
    │
    ├─→ Docker Image
    │   └─ Copied during build from repository root
    │
    └─→ crates.io
        └─ Specified in each crate's Cargo.toml
```

## Asset Distribution Matrix

| Asset | GitHub Release | npm Package | Docker Image | crates.io |
|-------|---------------|-------------|--------------|-----------|
| LICENSE | ✅ Included | ✅ Declared | ✅ Included | ✅ Specified |
| README.md | ✅ Included | ❌ Not needed | ✅ Included | ❌ N/A |
| orchestration/ | ✅ Included | ✅ Downloaded | ✅ Included | ❌ N/A |
| Binaries | ✅ Main content | ✅ Downloaded | ✅ Built in | ❌ N/A |

## Distribution Workflows

### 1. GitHub Releases (Primary Distribution)

**Trigger**: Git tag push (`v*`) or manual workflow dispatch

**Build Process** (`.github/workflows/release.yml`):
```yaml
# Step 1: Build binaries for all platforms
cargo build --release --bin agentflow --bin agentflow-setup --bin agentflow-dashboard --bin agentflow-doctor

# Step 2: Create distribution archive
mkdir -p dist/openflows-{version}-{platform}
cp target/release/agentflow* dist/openflows-{version}-{platform}/
cp -r orchestration dist/openflows-{version}-{platform}/
cp README.md LICENSE dist/openflows-{version}-{platform}/

# Step 3: Package and checksum
tar -czf openflows-{version}-{platform}.tar.gz
sha256sum openflows-{version}-{platform}.tar.gz > openflows-{version}-{platform}.tar.gz.sha256
```

**Platforms Built**:
- `x86_64-apple-darwin` (macOS Intel)
- `aarch64-apple-darwin` (macOS Apple Silicon)
- `x86_64-unknown-linux-musl` (Linux x86_64 static)
- `aarch64-unknown-linux-gnu` (Linux ARM64)

**Archive Contents**:
```
openflows-v0.1.3-x86_64-unknown-linux-musl/
├── agentflow                 # Main orchestration binary
├── agentflow-setup           # Interactive TUI setup wizard
├── agentflow-dashboard       # Real-time monitoring TUI
├── agentflow-doctor          # System diagnostics tool
├── orchestration/            # Agent configurations
│   ├── agent/
│   │   ├── agents/          # Agent persona definitions
│   │   │   ├── nexus.agent.md
│   │   │   ├── forge.agent.md
│   │   │   ├── sentinel.agent.md
│   │   │   └── vessel.agent.md
│   │   ├── registry.json     # Team membership configuration
│   │   └── standards/
│   └── plugin/
│       └── hooks/            # Per-agent lifecycle hooks
├── README.md                 # Documentation
└── LICENSE                   # MIT License
```

### 2. npm Package Distribution

**Package**: `@the-agenticflow/openflows`

**package.json Configuration**:
```json
{
  "name": "@the-agenticflow/openflows",
  "version": "0.1.2",
  "license": "MIT",
  "bin": {
    "openflows": "./bin/openflows.js",
    "openflows-setup": "./bin/openflows-setup.js",
    "openflows-dashboard": "./bin/openflows-dashboard.js",
    "openflows-doctor": "./bin/openflows-doctor.js"
  },
  "scripts": {
    "postinstall": "node scripts/install.js"
  }
}
```

**Post-Install Flow** (`packaging/npm/scripts/install.js`):

1. **Platform Detection**:
   - Detects OS: `darwin`, `linux`
   - Detects Arch: `x64`, `arm64`
   - Checks libc: `gnu` vs `musl`

2. **Binary Download**:
   ```javascript
   // Fetch latest release metadata
   GET https://api.github.com/repos/The-AgenticFlow/AgentFlow/releases/latest
   
   // Download platform-specific tarball
   GET https://github.com/The-AgenticFlow/AgentFlow/releases/download/{tag}/openflows-{tag}-{platform}.tar.gz
   
   // Extract to package bin/ directory
   tar -xzf openflows-{tag}-{platform}.tar.gz -C bin/
   ```

3. **Fallback Logic**:
   - If `x86_64-unknown-linux-gnu` fails, tries `x86_64-unknown-linux-musl`
   - Ensures compatibility across different Linux distributions

4. **Binary Renaming**:
   ```javascript
   // Rename for Node.js wrapper
   agentflow → agentflow-bin
   agentflow-setup → agentflow-setup-bin
   // etc.
   ```

**What Gets Distributed**:
- ✅ LICENSE: Declared in `package.json`
- ✅ Binaries: Downloaded from GitHub Releases (includes LICENSE + orchestration)
- ✅ orchestration/: Included in downloaded tarball
- ❌ README.md: Not included (users view on GitHub/npm registry)

**Installation Command**:
```bash
npm install -g @the-agenticflow/openflows
# or
npx @the-agenticflow/openflows setup
```

### 3. Docker Image Distribution

**Registry**: `ghcr.io/the-agenticflow/openflows`

**Build Process** (`.github/workflows/release.yml`):
```yaml
# Uses Docker Buildx for multi-arch builds
docker buildx build --push \
  --tag ghcr.io/the-agenticflow/openflows:latest \
  --tag ghcr.io/the-agenticflow/openflows:{version} \
  --platform linux/amd64,linux/arm64 \
  .
```

**Dockerfile** (`Dockerfile`):
```dockerfile
FROM rust:1.75 as builder
# Build all binaries
COPY . .
RUN cargo build --release --bin agentflow --bin agentflow-setup

FROM debian:bookworm-slim
# Copy binaries and assets
COPY --from=builder /app/target/release/agentflow /usr/local/bin/
COPY --from=builder /app/target/release/agentflow-setup /usr/local/bin/
COPY --from=builder /app/orchestration /app/orchestration/
COPY --from=builder /app/README.md /app/
COPY --from=builder /app/LICENSE /app/

# Runtime configuration
ENV RUST_LOG=info
CMD ["agentflow"]
```

**What Gets Distributed**:
- ✅ LICENSE: Copied to `/app/LICENSE`
- ✅ README.md: Copied to `/app/README.md`
- ✅ orchestration/: Copied to `/app/orchestration/`
- ✅ Binaries: Built into `/usr/local/bin/`

**Usage**:
```bash
# Pull and run
docker pull ghcr.io/the-agenticflow/openflows:latest
docker run -v $(pwd):/workspace ghcr.io/the-agenticflow/openflows

# With environment
docker run -e ANTHROPIC_API_KEY=... \
           -e GITHUB_PERSONAL_ACCESS_TOKEN=... \
           -v $(pwd):/workspace \
           ghcr.io/the-agenticflow/openflows
```

### 4. crates.io Distribution (Libraries Only)

**Published Crates**:
```yaml
# Build order (dependencies first)
- pocketflow-core
- config
- agent-client
- github
- agent-nexus
- agent-forge
- agent-sentinel
- agent-vessel
- agent-lore
- pair-harness
- agentflow-tui
- openflows (workspace root)
```

**License Declaration** (each `Cargo.toml`):
```toml
[package]
name = "pocketflow-core"
version = "0.1.0"
license = "MIT"
```

**What Gets Distributed**:
- ✅ LICENSE: Declared in each crate's metadata
- ❌ orchestration/: Not included (application-level config)
- ❌ README.md: Not included (crate-level docs are separate)
- ❌ Binaries: Not included (crates are libraries only)

**Publication Command**:
```bash
cargo publish -p pocketflow-core --token $CARGO_REGISTRY_TOKEN
cargo publish -p config --token $CARGO_REGISTRY_TOKEN
# ... for each crate
```

## Asset Lifecycle

### Development Phase
```
Repository
├── LICENSE (MIT)
├── README.md
├── orchestration/
│   ├── agent/registry.json
│   └── plugin/hooks/
└── crates/*/Cargo.toml (license = "MIT")
```

### Release Phase
```
Git Tag (v0.1.3)
    │
    ├──→ GitHub Actions: Build
    │    ├── Compile binaries (4 platforms)
    │    ├── Copy LICENSE + README.md + orchestration/
    │    ├── Create tarballs + checksums
    │    └── Upload to GitHub Releases
    │
    ├──→ GitHub Actions: Docker
    │    ├── Build multi-arch images
    │    ├── Embed LICENSE + README.md + orchestration/
    │    └── Push to ghcr.io
    │
    └──→ GitHub Actions: crates.io
         ├── Publish libraries
         └── Metadata includes MIT license
```

### Installation Phase (User)
```
npm install -g @the-agenticflow/openflows
    │
    ├── Download package.json (license: "MIT")
    ├── Run postinstall script
    │    ├── Detect platform
    │    ├── Download tarball from GitHub Releases
    │    │    └── Contains: binaries + LICENSE + orchestration/
    │    └── Extract to ~/.npm-global/lib/node_modules/@the-agenticflow/openflows/bin/
    │
    └── User runs:
         ├── openflows-setup → Uses orchestration/ from tarball
         └── openflows → Reads LICENSE embedded in binaries
```

## License Compliance

### For End Users
- **npm Package**: License declared in `package.json`, visible on npmjs.com
- **Docker Image**: LICENSE file at `/app/LICENSE`
- **GitHub Release**: LICENSE included in every tarball
- **Source Code**: LICENSE at repository root

### For Contributors
- **MIT License**: Permissive, allows commercial use
- **No CLA Required**: Inbound = Outbound licensing
- **Attribution**: Keep copyright notice + license text

### For Distributors
- **Requirement**: Include LICENSE with substantial portions
- **Allowed**: Sub-license, sell, modify, merge
- **Not Required**: Disclose source (MIT is not copyleft)

## Verification

### Verify License in Tarball
```bash
# Download tarball
curl -LO https://github.com/The-AgenticFlow/AgentFlow/releases/download/v0.1.3/openflows-v0.1.3-x86_64-unknown-linux-musl.tar.gz

# Extract and verify
tar -tzf openflows-v0.1.3-x86_64-unknown-linux-musl.tar.gz | grep LICENSE
# Expected: openflows-v0.1.3-x86_64-unknown-linux-musl/LICENSE

# View license
tar -xzf openflows-v0.1.3-x86_64-unknown-linux-musl.tar.gz
cat openflows-v0.1.3-x86_64-unknown-linux-musl/LICENSE
```

### Verify License in npm Package
```bash
# Install package
npm install -g @the-agenticflow/openflows

# Check package metadata
npm info @the-agenticflow/openflows license
# Expected: MIT

# View installed LICENSE (from downloaded tarball)
cat $(npm root -g)/@the-agenticflow/openflows/bin/LICENSE
```

### Verify License in Docker Image
```bash
# Pull image
docker pull ghcr.io/the-agenticflow/openflows:latest

# Inspect LICENSE file
docker run --rm ghcr.io/the-agenticflow/openflows:latest cat /app/LICENSE
```

### Verify License on crates.io
```bash
# Check crate metadata
cargo search pocketflow-core --limit 1
# Shows: pocketflow-core = "0.1.0" # ...

# View on crates.io
open https://crates.io/crates/pocketflow-core
# License field shows: MIT
```

## Version Updates

When creating a new release:

1. **Update LICENSE copyright year** (if needed):
   ```
   Copyright (c) 2026 Christian Yemele
   ```

2. **Ensure package.json version matches**:
   ```json
   {
     "version": "0.1.3",
     "license": "MIT"
   }
   ```

3. **Tag and release**:
   ```bash
   git tag -a v0.1.3 -m "Release v0.1.3"
   git push origin v0.1.3
   ```

4. **GitHub Actions automatically**:
   - Builds all platforms
   - Includes LICENSE in tarballs
   - Pushes Docker images with LICENSE
   - Publishes crates with MIT metadata

## Troubleshooting

### Missing LICENSE in tarball
- **Check**: `.github/workflows/release.yml` line 77
- **Expected**: `cp README.md LICENSE "dist/${ARCHIVE}/" 2>/dev/null || true`
- **Fix**: Ensure LICENSE exists at repository root

### npm install fails to download binary
- **Cause**: Network issue (like `EAI_AGAIN api.github.com`)
- **Solution 1**: Retry after network stabilizes
- **Solution 2**: Build from source instead
  ```bash
  git clone https://github.com/The-AgenticFlow/OpenFlows.git
  cd OpenFlows
  cargo build --release --bin agentflow-setup
  ```

### Docker image missing orchestration config
- **Check**: `Dockerfile` COPY commands
- **Expected**: `COPY --from=builder /app/orchestration /app/orchestration/`
- **Fix**: Rebuild image with correct COPY paths

## Related Documentation

- **Build Process**: `CONTRIBUTING.md` - Development setup
- **Release Process**: `PACKAGING.md` - Detailed packaging guide
- **Installation Guide**: `README.md` - User installation instructions
- **Configuration**: `orchestration/agent/README.md` - Agent configuration

## Summary

The MIT license and associated assets follow a consistent distribution pattern:

1. **Source of Truth**: Repository root LICENSE file
2. **Build-time Inclusion**: Assets copied into tarballs and Docker images
3. **Runtime Availability**: npm downloads tarballs containing LICENSE + orchestration
4. **Metadata Declaration**: All packages declare `license: "MIT"` in their manifests

This ensures license compliance across all distribution channels while maintaining a single source of truth in the repository.
