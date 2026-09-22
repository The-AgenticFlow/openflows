#!/bin/bash
# OpenFlows Production Commands
#
# Usage:
#   ./scripts/prod.sh bootstrap                    # Setup Coder + push templates
#   ./scripts/prod.sh tenant owner/repo --name team # Add a tenant
#   ./scripts/prod.sh doctor                       # Health check
#   ./scripts/prod.sh --help                       # Show help
#
# Note: there is no `run` command. The controller is started automatically,
# in-workspace, for each tenant when a tenant is added (its nexus workspace
# auto-starts the controller). Running the controller manually on the host has
# been removed as a duplicate of that in-workspace auto-start.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"

if [ -f "${PROJECT_ROOT}/.env" ]; then
    set -a
    source "${PROJECT_ROOT}/.env"
    set +a
fi

# Track whether the caller explicitly pinned a binary (env var or --bin flag).
# When explicit, we respect it as-is and never touch it. Otherwise we always
# rebuild the in-repo release binary from source before every invocation, so
# the controller can never silently keep running against stale code just
# because nobody remembered to rebuild after a fix (cargo is incremental —
# this is a no-op in well under a second when nothing changed).
if [ -n "${OPENFLOWS_BIN:-}" ]; then
    OPENFLOWS_BIN_EXPLICIT=1
else
    OPENFLOWS_BIN="openflows"
    OPENFLOWS_BIN_EXPLICIT=0
fi
SKIP_BUILD=false
BUILD_DONE=false

usage() {
    cat <<'USAGE'
OpenFlows Production Commands

Usage:
  ./scripts/prod.sh bootstrap                          Setup Coder + push templates
  ./scripts/prod.sh tenant owner/repo --name team-name  Add a tenant (each tenant runs its own in-workspace controller)
  ./scripts/prod.sh doctor                             Health check

Options:
  --bin PATH     Use this exact openflows binary and skip auto-rebuild
  --skip-build   Skip the automatic `cargo build --release` before running
                 (uses whatever binary find_binary() resolves to, which may
                 be stale — only use this if you know what you're doing)

Note: by default, every invocation rebuilds ./target/release/openflows from
current source first (cargo build is incremental, so this is fast when
nothing changed). This guarantees the binary never silently runs on a stale
build after a code fix.

The controller itself is not run here — it auto-starts in each tenant's nexus
workspace when the tenant is added. `prod.sh` only boots Coder, adds tenants,
and runs health checks.

Examples:
  # First-time setup:
  ./scripts/prod.sh bootstrap

  # Add a team (provisions the tenant's nexus workspace + auto-started controller):
  ./scripts/prod.sh tenant my-org/my-repo --name my-team

  # Health check:
  ./scripts/prod.sh doctor

USAGE
}

# Ensure the in-repo release binary is built from the CURRENT source tree.
# `cargo build` is incremental, so when nothing changed this completes almost
# instantly — but when the source HAS changed (e.g. a bugfix was just applied),
# this guarantees prod.sh never silently runs stale, previously-built code.
# This is what closes the gap that let a fixed bug keep reproducing because an
# old ./target/release/openflows binary was reused across sessions.
ensure_fresh_binary() {
    if [ "$BUILD_DONE" = "true" ]; then
        return
    fi
    echo "  → Ensuring openflows binary is built from current source..."
    if ! (cd "$PROJECT_ROOT" && cargo build --release -p openflows) ; then
        echo "❌ Failed to build openflows from source" >&2
        exit 1
    fi
    BUILD_DONE=true
}

# Find openflows binary
find_binary() {
    if [ -x "$OPENFLOWS_BIN" ]; then
        echo "$OPENFLOWS_BIN"
    elif [ -x "./target/release/openflows" ]; then
        echo "./target/release/openflows"
    elif command -v openflows >/dev/null 2>&1; then
        echo "openflows"
    else
        echo "openflows"  # Let it fail with proper error
    fi
}

run_openflows() {
    local cmd="$1"
    shift

    if [ "$OPENFLOWS_BIN_EXPLICIT" = "1" ]; then
        echo "  → Using explicitly pinned binary: $OPENFLOWS_BIN (skipping auto-rebuild)"
    elif [ "$SKIP_BUILD" = "true" ]; then
        echo "  → Skipping auto-rebuild (--skip-build)"
    else
        ensure_fresh_binary
        OPENFLOWS_BIN="${PROJECT_ROOT}/target/release/openflows"
    fi

    local bin
    bin=$(find_binary)
    if ! command -v "$bin" >/dev/null 2>&1 && [ ! -x "$bin" ]; then
        echo "❌ openflows binary not found"
        echo ""
        echo "Install it with:"
        echo "  curl -fsSL https://get.openflows.dev | bash"
        echo ""
        echo "Or build from source:"
        echo "  cargo build --release -p openflows"
        exit 1
    fi
    "$bin" "$cmd" "$@"
}

# Parse global options
while [[ $# -gt 0 ]]; do
    case "$1" in
        --bin)
            OPENFLOWS_BIN="$2"
            OPENFLOWS_BIN_EXPLICIT=1
            shift 2
            ;;
        --skip-build)
            SKIP_BUILD=true
            shift
            ;;
        --help|-h)
            usage
            exit 0
            ;;
        --)
            shift
            break
            ;;
        -*)
            echo "❌ Unknown option: $1"
            usage
            exit 1
            ;;
        *)
            break
            ;;
    esac
done

# Run command
CMD="${1:-}"
shift || true

case "$CMD" in
    bootstrap)
        echo "═══════════════════════════════════════"
        echo "  OpenFlows Bootstrap"
        echo "═══════════════════════════════════════"
        echo ""
        echo "Step 1: Syncing dev binary..."
        if [ -f "${SCRIPT_DIR}/dev-sync.sh" ]; then
            "${SCRIPT_DIR}/dev-sync.sh"
        else
            echo "⚠ dev-sync.sh not found, skipping..."
        fi
        echo ""
        echo "Step 2: Running Coder bootstrap..."
        echo ""
        echo "This will:"
        echo "  ✓ Create admin user in Coder"
        echo "  ✓ Push workspace templates (nexus, forge, etc.)"
        echo "  ✓ Verify LLM and GitHub auth are configured"
        echo ""
        run_openflows bootstrap "$@"
        ;;

    tenant)
        if [ -z "${1:-}" ]; then
            echo "❌ Missing owner/repo argument"
            echo ""
            echo "Usage: ./scripts/prod.sh tenant owner/repo --name team-name"
            echo ""
            echo "Example: ./scripts/prod.sh tenant my-org/my-repo --name my-team"
            exit 1
        fi
        OWNER_REPO="$1"
        shift

        NAME=""
        while [[ $# -gt 0 ]]; do
            case "$1" in
                --name)
                    NAME="$2"
                    shift 2
                    ;;
                *)
                    shift
                    ;;
            esac
        done

        if [ -z "$NAME" ]; then
            echo "❌ Missing --name argument"
            echo ""
            echo "Usage: ./scripts/prod.sh tenant owner/repo --name team-name"
            exit 1
        fi

        echo "═══════════════════════════════════════"
        echo "  OpenFlows: Adding Tenant"
        echo "═══════════════════════════════════════"
        echo ""
        echo "  Owner/Repo: $OWNER_REPO"
        echo "  Tenant Name: $NAME"
        echo ""
        run_openflows tenant add "$OWNER_REPO" --name "$NAME" "$@"
        ;;

    doctor)
        echo "═══════════════════════════════════════"
        echo "  OpenFlows Health Check"
        echo "═══════════════════════════════════════"
        echo ""
        run_openflows doctor "$@"
        ;;

    help|--help|-h|"")
        usage
        exit 0
        ;;

    *)
        echo "❌ Unknown command: $CMD"
        echo ""
        usage
        exit 1
        ;;
esac