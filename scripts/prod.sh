#!/bin/bash
# OpenFlows Production Commands
#
# Usage:
#   ./scripts/prod.sh run                          # Clean slate + start controller
#   ./scripts/prod.sh bootstrap                    # Setup Coder + push templates
#   ./scripts/prod.sh tenant owner/repo --name team # Add a tenant
#   ./scripts/prod.sh doctor                       # Health check
#   ./scripts/prod.sh --help                       # Show help

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OPENFLOWS_BIN="${OPENFLOWS_BIN:-openflows}"

usage() {
    cat <<'USAGE'
OpenFlows Production Commands

Usage:
  ./scripts/prod.sh run                                Clean slate + start controller
  ./scripts/prod.sh bootstrap                          Setup Coder + push templates
  ./scripts/prod.sh tenant owner/repo --name team-name  Add a tenant
  ./scripts/prod.sh doctor                             Health check

Options:
  --bin PATH    Path to openflows binary (default: openflows or ./target/release/openflows)

Examples:
  # Start controller (always resets state first):
  ./scripts/prod.sh run

  # First-time setup:
  ./scripts/prod.sh bootstrap

  # Add a team:
  ./scripts/prod.sh tenant my-org/my-repo --name my-team

  # Health check:
  ./scripts/prod.sh doctor

USAGE
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
            shift 2
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
    run)
        echo "═══════════════════════════════════════"
        echo "  OpenFlows: Starting Controller"
        echo "═══════════════════════════════════════"
        echo ""
        echo "Step 1: Resetting Redis state (clean slate)..."
        if [ -f "${SCRIPT_DIR}/reset-controller-state.sh" ]; then
            "${SCRIPT_DIR}/reset-controller-state.sh" --confirm
        else
            echo "⚠ reset-controller-state.sh not found, skipping..."
        fi
        echo ""
        echo "Step 2: Starting OpenFlows controller..."
        echo ""
        run_openflows run "$@"
        ;;

    bootstrap)
        echo "═══════════════════════════════════════"
        echo "  OpenFlows Bootstrap"
        echo "═══════════════════════════════════════"
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