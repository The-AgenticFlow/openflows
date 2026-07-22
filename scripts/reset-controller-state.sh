#!/bin/bash
# Reset OpenFlows controller state to a clean slate for testing.
# This clears Redis keys related to tickets, workers, and PRs, but preserves
# CI setup flags and other operational metadata. Zombie workspaces in Coder
# must be cleaned up separately (coder delete-workspace).
#
# Usage:
#   ./scripts/reset-controller-state.sh              # interactive confirmation
#   ./scripts/reset-controller-state.sh --confirm    # skip confirmation
#   ./scripts/reset-controller-state.sh --full       # also delete all Coder workspaces

set -e

CONFIRM="${1:-}"
REDIS_CONTAINER="openflows-redis-1"
REDIS_URL="${REDIS_URL:-redis://localhost:6379}"

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

if [ "$CONFIRM" != "--confirm" ] && [ "$CONFIRM" != "--full" ]; then
    read -p "Continue? (y/n) " -n 1 -r
    echo ""
    if [[ ! $REPLY =~ ^[Yy]$ ]]; then
        echo "Aborted."
        exit 0
    fi
fi

echo "Clearing Redis keys..."

# Delete all ticket-related keys (ticket:*, heartbeat:*)
redis_cmd EVAL "
local keys = redis.call('KEYS', 'ticket:*')
for i, k in ipairs(keys) do redis.call('DEL', k) end
keys = redis.call('KEYS', 'heartbeat:*')
for i, k in ipairs(keys) do redis.call('DEL', k) end
return #keys
" 0 > /dev/null 2>&1 || true

# Delete worker and PR state
for key in worker_slots pending_prs open_prs command_gate _no_work_count; do
    redis_cmd DEL "$key" >/dev/null 2>&1 || true
done

# Count remaining keys
REMAINING=$(redis_cmd DBSIZE | grep -oE '[0-9]+' || echo "0")
echo "Cleared ticket and worker state. $REMAINING key(s) remain (preserved)."

# Keep tickets list for GitHub sync, but wipe its contents
echo "Resetting tickets list..."
redis_cmd DEL "tickets" >/dev/null 2>&1 || true
redis_cmd SET "tickets" "[]" >/dev/null 2>&1 || true

echo ""
echo "Redis state cleared."
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
