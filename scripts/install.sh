#!/usr/bin/env bash
# OpenFlows Installer
# Usage: curl -fsSL https://get.openflows.dev | bash
#   or:  curl -fsSL https://raw.githubusercontent.com/The-AgenticFlow/AgentFlow/main/scripts/install.sh | bash

set -euo pipefail

REPO="The-AgenticFlow/AgentFlow"
INSTALL_DIR="${AGENTFLOW_INSTALL_DIR:-$HOME/.local/bin}"
BINARIES=("openflows" "openflows-setup" "openflows-dashboard" "openflows-doctor")

# Colors
RED='\033[0;31m'
GREEN='\033[0;32m'
YELLOW='\033[1;33m'
BLUE='\033[0;34m'
NC='\033[0m'

info()    { echo -e "${BLUE}  →${NC} $1"; }
success() { echo -e "${GREEN}  ✓${NC} $1"; }
warn()    { echo -e "${YELLOW}  ⚠${NC} $1"; }
fail()    { echo -e "${RED}  ✗${NC} $1" >&2; }

# Detect OS and architecture
detect_platform() {
    local os arch
    os=$(uname -s | tr '[:upper:]' '[:lower:]')
    arch=$(uname -m)
    case "$arch" in
        x86_64) arch="x86_64" ;;
        aarch64|arm64) arch="aarch64" ;;
        *) fail "Unsupported architecture: $arch"; exit 1 ;;
    esac
    case "$os" in
        darwin) os="apple-darwin" ;;
        linux)
            # Prefer musl for portability
            if ldd --version 2>&1 | grep -qi musl; then
                os="unknown-linux-musl"
            else
                os="unknown-linux-gnu"
            fi
            ;;
        *) fail "Unsupported OS: $os"; exit 1 ;;
    esac
    echo "${arch}-${os}"
}

# Check if a command exists
has_cmd() { command -v "$1" &>/dev/null; }

# Check/install Rust toolchain
ensure_rust() {
    if has_cmd rustc; then
        success "Rust $(rustc --version)"
        return
    fi
    info "Rust not found. Installing rustup..."
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
    if [ -f "$HOME/.cargo/env" ]; then
        source "$HOME/.cargo/env"
    fi
    success "Rust $(rustc --version)"
}

# Check Git
ensure_git() {
    if has_cmd git; then
        success "Git $(git --version)"
    else
        fail "Git is required. Please install git first."
        exit 1
    fi
}

# Check/install Node.js
ensure_node() {
    if has_cmd node; then
        local node_ver
        node_ver=$(node --version)
        local major
        major=$(echo "$node_ver" | cut -d. -f1 | tr -d 'v')
        if [ "$major" -ge 18 ]; then
            success "Node.js $node_ver"
            return
        fi
        warn "Node.js $node_ver is too old (need 18+). Installing via nvm..."
    else
        info "Node.js not found. Installing via nvm..."
    fi
    curl -o- https://raw.githubusercontent.com/nvm-sh/nvm/v0.40.1/install.sh | bash
    export NVM_DIR="$HOME/.nvm"
    [ -s "$NVM_DIR/nvm.sh" ] && . "$NVM_DIR/nvm.sh"
    nvm install 20
    success "Node.js $(node --version)"
}

# Check for AI CLI backend (Claude Code or Codex)
ensure_ai_cli() {
    local has_claude=false
    local has_codex=false

    if has_cmd claude; then
        success "Claude Code CLI $(claude --version 2>/dev/null || echo 'installed')"
        has_claude=true
    fi

    if has_cmd codex; then
        success "Codex CLI $(codex --version 2>/dev/null || echo 'installed')"
        has_codex=true
    fi

    if [ "$has_claude" = false ] && [ "$has_codex" = false ]; then
        warn "No AI CLI backend found."
        echo ""
        echo "  OpenFlows requires either Claude Code (Anthropic) or Codex (OpenAI)."
        echo ""
        echo "  To install Claude Code:"
        echo "    npm install -g @anthropic-ai/claude-code"
        echo "    claude login"
        echo ""
        echo "  To install Codex:"
        echo "    npm install -g @openai/codex"
        echo "    codex login"
        echo ""
        echo "  You can also use npx without installing globally:"
        echo "    npx @anthropic-ai/claude-code"
        echo "    npx @openai/codex"
        echo ""
        info "Continuing without AI CLI - you'll need to install one before running OpenFlows"
    fi
}

# Download pre-built binary from GitHub Releases
download_binary() {
    local platform="$1"
    local tag="$2"
    local asset_name="openflows-${tag}-${platform}.tar.gz"

    info "Downloading OpenFlows ${tag} for ${platform}..."

    local download_url="https://github.com/${REPO}/releases/download/${tag}/${asset_name}"

    if has_cmd curl; then
        curl -fsSL "$download_url" -o "/tmp/${asset_name}" || {
            fail "Failed to download ${asset_name}"
            info "Falling back to building from source..."
            return 1
        }
    elif has_cmd wget; then
        wget -q "$download_url" -O "/tmp/${asset_name}" || {
            fail "Failed to download ${asset_name}"
            info "Falling back to building from source..."
            return 1
        }
    else
        fail "Neither curl nor wget found"
        return 1
    fi

    tar -xzf "/tmp/${asset_name}" -C /tmp/
    rm -f "/tmp/${asset_name}"

    local extract_dir="/tmp/openflows-${tag}-${platform}"
    for bin in "${BINARIES[@]}"; do
        if [ -f "${extract_dir}/${bin}" ]; then
            cp "${extract_dir}/${bin}" "${INSTALL_DIR}/"
            chmod +x "${INSTALL_DIR}/${bin}"
        fi
    done
    rm -rf "${extract_dir}"

    success "Downloaded and extracted to ${INSTALL_DIR}/"
    return 0
}

# Build from source as fallback
build_from_source() {
    info "Building OpenFlows from source..."
    local repo_dir
    repo_dir=$(mktemp -d)
    trap "rm -rf '$repo_dir'" EXIT

    git clone --depth 1 "https://github.com/${REPO}.git" "$repo_dir"
    cd "$repo_dir"

    cargo build --release -p openflows

    for bin in "${BINARIES[@]}"; do
        if [ -f "target/release/${bin}" ]; then
            cp "target/release/${bin}" "${INSTALL_DIR}/"
            chmod +x "${INSTALL_DIR}/${bin}"
            success "Built and installed ${bin}"
        fi
    done
}

# Add install dir to PATH if needed
ensure_path() {
    if ! echo "$PATH" | tr ':' '\n' | grep -qx "$INSTALL_DIR"; then
        warn "${INSTALL_DIR} is not in your PATH"
        local shell_rc=""
        case "$SHELL" in
            */bash)  shell_rc="$HOME/.bashrc" ;;
            */zsh)   shell_rc="$HOME/.zshrc" ;;
            */fish)  shell_rc="$HOME/.config/fish/config.fish" ;;
        esac
        if [ -n "$shell_rc" ]; then
            if [[ "$SHELL" == */fish ]]; then
                echo "fish_add_path $INSTALL_DIR" >> "$shell_rc"
            else
                echo "export PATH=\"\$PATH:$INSTALL_DIR\"" >> "$shell_rc"
            fi
            info "Added $INSTALL_DIR to PATH in $shell_rc"
            info "Run 'source $shell_rc' or restart your terminal"
        fi
    fi
}

# Offer to run setup wizard
run_setup() {
    echo ""
    echo "Would you like to run the setup wizard now? (Y/n)"
    read -r response
    if [[ "$response" =~ ^[Yy]$ ]] || [[ -z "$response" ]]; then
        if has_cmd openflows-setup; then
            openflows-setup
        else
            warn "openflows-setup not found in PATH. Run it manually after adding $INSTALL_DIR to PATH."
        fi
    fi
}

# Main
main() {
    echo ""
    echo "╔══════════════════════════════════════════════╗"
    echo "║        OpenFlows Installer                   ║"
    echo "║        Autonomous AI Development Team        ║"
    echo "╚══════════════════════════════════════════════╝"
    echo ""

    local platform
    platform=$(detect_platform)
    info "Platform: $platform"
    info "Install directory: $INSTALL_DIR"
    echo ""

    # Check prerequisites
    info "Checking prerequisites..."
    ensure_rust
    ensure_git
    ensure_node
    ensure_ai_cli
    echo ""

    # Try to download pre-built binary
    mkdir -p "$INSTALL_DIR"
    local installed=false

    if has_cmd gh; then
        local latest
        latest=$(gh release view --repo "$REPO" --json tagName -q .tagName 2>/dev/null || echo "")
        if [ -n "$latest" ]; then
            if download_binary "$platform" "$latest"; then
                installed=true
            fi
        fi
    fi

    if [ "$installed" = false ]; then
        # Try direct download without gh CLI
        local latest
        latest=$(curl -fsSL "https://api.github.com/repos/${REPO}/releases/latest" 2>/dev/null | grep '"tag_name"' | head -1 | sed 's/.*"tag_name": "//;s/".*//' || echo "")
        if [ -n "$latest" ]; then
            if download_binary "$platform" "$latest"; then
                installed=true
            fi
        fi
    fi

    if [ "$installed" = false ]; then
        build_from_source
    fi

    echo ""
    ensure_path

    echo ""
    echo "╔══════════════════════════════════════════════╗"
    echo "║        Installation Complete!                ║"
    echo "╚══════════════════════════════════════════════╝"
    echo ""
    echo "  Available commands:"
    echo "    openflows         - Start orchestration"
    echo "    openflows-setup   - Guided setup wizard"
    echo "    openflows-dashboard - Live monitoring TUI"
    echo "    openflows-doctor  - Diagnostic checks"
    echo ""
    echo "  Next steps:"
    echo "    1. Run 'openflows-setup' to configure API keys"
    echo "    2. Run 'openflows' to start the autonomous team"
    echo ""
    echo "  Docs: https://openflows.dev"
    echo ""

    run_setup
}

main "$@"
