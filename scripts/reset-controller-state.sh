#!/bin/bash
# Reset OpenFlows controller state to a clean slate for testing.
# This clears Redis keys related to tickets, workers, and PRs, but preserves
# CI setup flags and other operational metadata. Zombie workspaces in Coder
# must be cleaned up separately (coder delete-workspace).
#
# Usage:
#   ./scripts/reset-controller-state.sh              # interactive confirmation
#   ./scripts/reset-controller-state.sh --confirm   # skip confirmation (default tenant)
#   ./scripts/reset-controller-state.sh --all        # reset ALL tenants
#   ./scripts/reset-controller-state.sh --full      # also delete all Coder workspaces

set -e

CONFIRM="${1:-}"
RESET_TENANT="${OPENFLOWS_TENANT:-default}"
REDIS_CONTAINER="openflows-redis-1"
REDIS_URL="${REDIS_URL:-redis://localhost:6379}"
OPENFLOWS_BIN="${OPENFLOWS_BIN:-./target/release/openflows}"

# Check if Redis container is running
if ! docker ps | grep -q "$REDIS_CONTAINER"; then
    echo "ERROR: Redis container '$REDIS_CONTAINER' is not running."
    echo "Start it with: docker-compose up -d"
    exit 1
fi

redis_cmd() {
    docker exec "$REDIS_CONTAINER" redis-cli "$@"
}

echo "=== OpenFlows Controller State Reset ==="
echo ""
echo "This will clear:"
echo "  • All tickets and ticket metadata (dispatch, status, chat, recovery_attempts, etc.)"
echo "  • All worker slots (but NOT workspace IDs in Coder)"
echo "  • Pending PRs and command gate state"
echo "  • All heartbeat records"
echo ""
echo "This will PRESERVE:"
echo "  • CI readiness state (CI setup tickets will survive)"
echo "  • GitHub sync metadata (issues, PRs, branches)"
echo ""

if [ "$CONFIRM" != "--confirm" ] && [ "$CONFIRM" != "--all" ] && [ "$CONFIRM" != "--full" ]; then
    read -p "Continue? (y/n) " -n 1 -r
    echo ""
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 0
    fi
fi

echo "Cleaning Redis state..."

# Use openflows binary's tenant clean command if available
# This intelligently resets stale tickets without losing GitHub sync metadata
if [ -x "$OPENFLOWS_BIN" ] || command -v "$OPENFLOWS_BIN" &>/dev/null; then
    if [ "$CONFIRM" == "--all" ]; then
        # List all tenants and clean each one
        TENANTS=$("$OPENFLOWS_BIN" tenant list 2>/dev/null | grep "^  -" | sed 's/  - //')
        for TENANT in $TENANTS; do
            echo "Cleaning tenant: $TENANT"
            OPENFLOWS_TENANT="$TENANT" "$OPENFLOWS_BIN" tenant clean "$TENANT" --reset-all 2>/dev/null || true
        done
    else
        # Clean default/selected tenant
        echo "Cleaning tenant: $RESET_TENANT"
        OPENFLOWS_TENANT="$RESET_TENANT" "$OPENFLOWS_BIN" tenant clean "$RESET_TENANT" --reset-all 2>&1 || true
    fi
else
    # Fallback to direct Redis cleanup (legacy mode)
    echo "openflows binary not found, using direct Redis cleanup..."

    # Delete all ticket-related keys including namespaced keys (ns:*:ticket:*, ns:*:heartbeat:*)
    # Also reset tickets with stale status (awaiting_human, failed with max attempts) back to Open
    redis_cmd EVAL "
    -- Clear un_namespaced keys
    local keys = redis.call('KEYS', 'ticket:*')
    for i, k in ipairs(keys) do redis.call('DEL', k) end
    keys = redis.call('KEYS', 'heartbeat:*')
    for i, k in ipairs(keys) do redis.call('DEL', k) end

    -- Clear namespaced keys (all tenants)
    keys = redis.call('KEYS', 'ns:*:ticket:*')
    for i, k in ipairs(keys) do redis.call('DEL', k) end
    keys = redis.call('KEYS', 'ns:*:heartbeat:*')
    for i, k in ipairs(keys) do redis.call('DEL', k) end

    -- Clear namespaced tickets lists prefix
    return 'done'
    " 0 > /dev/null 2>&1 || true

    # Delete worker and PR state (both un_namespaced and namespaced)
    for key in worker_slots pending_prs open_prs command_gate _no_work_count; do
        redis_cmd DEL "$key" >/dev/null 2>&1 || true
        redis_cmd EVAL "local keys = redis.call('KEYS', 'ns:*:${key}') for i, k in ipairs(keys) do redis.call('DEL', k) end return 0" 0 >/dev/null 2>&1 || true
    done

    # Reset un_namespaced tickets list
    redis_cmd DEL "tickets" >/dev/null 2>&1 || true
    redis_cmd SET "tickets" "[]" >/dev/null 2>&1 || true
fi

# Count remaining keys
REMAINING=$(redis_cmd DBSIZE | grep -oE '[0-9]+' || echo "0")
echo "Cleaned state. $REMAINING key(s) remain (preserved)."

echo ""
echo "State cleared."
echo ""

if [ "$CONFIRM" = "--full" ]; then
    echo "=== Cleaning up Coder Workspaces ==="
    echo "Note: Requires CODER_URL and CODER_SESSION_TOKEN to be set."
    
    if [ -z "$CODER_URL" ] || [ -z "$CODER_SESSION_TOKEN" ]; then
        echo "ERROR: CODER_URL and/or CODER_SESSION_TOKEN not set."
        echo "Set them with: export CODER_URL=... CODER_SESSION_TOKEN=..."
        exit 1
    fi
    
    echo "Querying Coder for openflows-forge-* workspaces..."
    # This is a best-effort attempt — requires the coder CLI
    if command -v coder >/dev/null; then
        WORKSPACES=$(coder list --offline 2>/dev/null | grep "openflows-forge" | awk '{print $1}' || true)
        if [ -n "$WORKSPACES" ]; then
            echo "Found workspaces:"
            echo "$WORKSPACES" | sed 's/^/  /'
            echo ""
            read -p "Delete these workspaces? (y/n) " -n 1 -r
            echo ""
            if [[ $REPLY =~ ^[Yy]$ ]]; then
                echo "$WORKSPACES" | while read -r ws; do
                    echo "Deleting $ws..."
                    coder delete "$ws" --force 2>/dev/null || echo "  (failed or skipped)"
                done
            fi
        else
            echo "No openflows-forge workspaces found."
        fi
    else
        echo "WARNING: 'coder' CLI not found. Workspaces must be deleted manually:"
        echo "  coder list --offline"
        echo "  coder delete <workspace-name> --force"
    fi
fi

echo ""
echo "=== Clean State Ready ==="
echo "You can now:"
echo "  1. Start the controller: cargo run -p openflows --bin openflows"
echo "  2. Create a test ticket in GitHub"
echo "  3. Monitor the logs: tail -f /tmp/openflows-controller.log"
echo ""
