#!/usr/bin/env bash
# Start the local Anthropic-to-OpenAI proxy.
#
# This proxy translates Anthropic Messages API requests (used by Claude CLI)
# into OpenAI Chat Completions format, forwarding to GATEWAY_URL.
#
# The proxy loads .env automatically via dotenvy — but we also source it here
# to get PORT before spawning the process.
#
# Required env vars (set in .env):
#   GATEWAY_URL      - Remote OpenAI-compatible gateway (e.g., https://api.ai.camer.digital/v1/)
#   GATEWAY_API_KEY  - API key for the gateway
#
# Optional env vars:
#   PORT  - Local port (default: 8765 to avoid conflicts with app servers)
#
# Usage:
#   ./scripts/start_proxy.sh
#   # Then in another terminal: cargo run --bin real_test

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
PROJECT_DIR="$(dirname "$SCRIPT_DIR")"

cd "$PROJECT_DIR"

# Source .env to get PORT and other config
if [ -f .env ]; then
    set -a
    source .env
    set +a
fi

# Build if needed
if [ ! -f target/debug/anthropic-proxy ] || [ "$(find crates/anthropic-mock/src -newer target/debug/anthropic-proxy 2>/dev/null | wc -l)" -gt 0 ]; then
    echo "Building anthropic-proxy..."
    cargo build -p anthropic-proxy
fi

# Default to 8765 to avoid conflicts with application servers (Actix, Express, etc.)
PORT="${PORT:-8765}"

echo "Starting Anthropic-to-OpenAI proxy on :${PORT}"
echo "  The proxy reads GATEWAY_URL and GATEWAY_API_KEY from .env"
echo "  Press Ctrl+C to stop"
echo ""

PORT="$PORT" \
RUST_LOG="${RUST_LOG:-info}" \
exec target/debug/anthropic-proxy
