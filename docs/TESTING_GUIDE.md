# Testing OpenFlows with Clean State

## Quick Start

Before running tests, reset the controller state to avoid zombie workspaces and stale tickets interfering:

```bash
# Interactive reset (asks for confirmation)
./scripts/reset-controller-state.sh

# Skip confirmation (for CI/automation)
./scripts/reset-controller-state.sh --confirm

# Also delete all Coder workspaces (requires coder CLI + CODER_URL/TOKEN)
./scripts/reset-controller-state.sh --full
```

## Redis Key Structure

The controller stores state in Redis under these keys:

### Root-Level Keys (namespace-wide)
| Key | Type | Purpose |
|-----|------|---------|
| `tickets` | JSON array | All active tickets from GitHub |
| `worker_slots` | JSON map | Worker status (forge-1, sentinel-1, etc.) |
| `pending_prs` | JSON array | Open PRs awaiting merge |
| `command_gate` | JSON map | Suspended workers awaiting human approval |
| `ci_readiness` | JSON | CI setup status (setup_missing, setup_in_progress, ready) |
| `repository` | string | GitHub repo URL (owner/name) |

### Per-Ticket Keys
All per-ticket keys use the pattern `ticket:{ticket_id}:{suffix}`:

| Key Suffix | Type | Purpose |
|------------|------|---------|
| `dispatch:{role}` | JSON | Task payload (forge reads this) |
| `status` | JSON | Current phase (planning, building, testing, review_ready, blocked) |
| `chat:{role}` | string | Coder chat ID |
| `chat_action:{role}` | string | Last chat action (created, follow_up_sent, interrupted, completed) |
| `workspace:{role}` | string | Coder workspace ID |
| `pr` | JSON | PR info (pr_number, branch, title) |
| `handoff` | JSON | Forge→Sentinel handoff contract |
| `review:{role}` | JSON | Sentinel review verdict |
| `deployment` | JSON | Merge metadata (merged PR #, SHA) |
| `recovery_attempts` | number | Crash/escalation attempt count (0-3) |

### Heartbeat Keys
Pattern: `heartbeat:{role}-{ticket_id}`
- Written by: `openflows-harness heartbeat start` (every 30s)
- TTL: 120s (auto-expires if workspace goes silent)
- Value: `{"ts": <unix-ms>, "ws_id": "<workspace-id>", "status": "running"}`
- Read by: `inspect_coder_recovery` to detect workspace crashes

## Inspecting State

### Using Docker Redis CLI

```bash
# Connect to Redis and inspect keys
docker exec openflows-redis-1 redis-cli

# Inside redis-cli:
KEYS "ticket:T-*"           # All ticket keys
GET "tickets"                # Full tickets list
GET "worker_slots"           # Worker status
KEYS "heartbeat:*"           # Active heartbeats
GET "ticket:T-042:status"    # Status of T-042
GET "ticket:T-042:dispatch:forge"  # Forge task payload
```

### Using a Monitoring Script

```bash
# Watch Redis state in real-time
watch -n 2 'docker exec openflows-redis-1 redis-cli KEYS "*" | wc -l'

# Get worker status
docker exec openflows-redis-1 redis-cli GET "worker_slots" | jq .

# Get all tickets
docker exec openflows-redis-1 redis-cli GET "tickets" | jq .
```

## Workflow: Testing a Fresh Ticket

1. **Reset state** to start clean:
   ```bash
   ./scripts/reset-controller-state.sh --confirm
   ```

2. **Create a test issue** in GitHub (or use an existing one):
   ```bash
   # The controller polls issues every 15s
   # Watch the controller log to see it sync
   tail -f /tmp/openflows-controller.log | grep "sync_issues"
   ```

3. **Monitor worker assignment**:
   ```bash
   tail -f /tmp/openflows-controller.log | grep "Nexus:"
   # Look for: "dispatching assignable ticket to an idle forge worker"
   ```

4. **Check workspace provisioning**:
   ```bash
   tail -f /tmp/openflows-controller.log | grep "Provisioning Coder workspace"
   # Look for: "Coder workspace provisioned"
   ```

5. **Inspect workspace startup** (from workspace terminal):
   ```bash
   # Inside the workspace:
   openflows-harness dispatch read      # See the task payload
   openflows-harness status get         # See current phase
   ```

## Debugging Common Issues

### "Workspace not yet provisioned" (forge-2 stuck)

This means provisioning failed silently. Check:

```bash
# Look for provisioning errors in controller log
tail -f /tmp/openflows-controller.log | grep -E "provision|Failed to provision|workspace_id"

# Check if Coder workspaces were created at all
coder list --offline | grep openflows

# Check Coder status (templates, orgs, quotas)
coder stat

# Verify CODER_URL and token are set in the controller environment
env | grep CODER_
```

### "Recovery limit reached — escalating" (forge-1 re-escalating every 15s)

This means the workspace crashed and recovery is stuck. Check:

```bash
# Look for workspace crash reasons
tail -f /tmp/openflows-controller.log | grep -E "heartbeat stale|workspace.*failed|agent status"

# Check heartbeat presence
docker exec openflows-redis-1 redis-cli KEYS "heartbeat:*"

# Inspect the crashed workspace
coder list --offline | grep T-XXX
coder ssh <workspace-name> -- 'openflows-harness heartbeat stop'
```

### "no idle forge worker — pausing" (fleet stuck)

Both workers are busy. This should clear within seconds if the fixes work:

```bash
# Check worker statuses
docker exec openflows-redis-1 redis-cli GET "worker_slots" | jq .

# Expected: both forge-* have status=Assigned or status=Working
# If yes, check their recovery_attempts and ticket status:
for ticket in T-047 T-048 T-049; do
  echo "=== $ticket ==="
  docker exec openflows-redis-1 redis-cli GET "ticket:$ticket:recovery_attempts"
  docker exec openflows-redis-1 redis-cli GET "ticket:$ticket:status"
done

# If a ticket is AwaitingHuman, the slot should have been released.
# If it's still Assigned, that's the bug—file an issue.
```

## Full Reset Sequence

To start completely from scratch (including deleting workspaces):

```bash
# Reset Redis state
./scripts/reset-controller-state.sh --confirm

# Stop the controller if running
pkill -f "agentflow" || true
sleep 2

# Delete all Coder workspaces (requires coder CLI)
coder list --offline | grep openflows-forge | awk '{print $1}' | \
  xargs -I {} coder delete {} --force

# Verify workspaces are gone
coder list --offline | grep openflows || echo "No workspaces found ✓"

# Restart the controller
cargo run -p openflows --bin agentflow &
disown

# Monitor logs
tail -f /tmp/openflows-controller.log
```

## Environment Variables for Testing

Set these before running tests:

```bash
# GitHub
export GITHUB_TOKEN="ghp_..."
export GITHUB_REPOSITORY="owner/repo"

# Coder
export CODER_URL="https://coder.example.com"
export CODER_SESSION_TOKEN="..."  # or CODER_API_TOKEN

# Redis
export REDIS_URL="redis://localhost:6379"

# OpenFlows
export OPENFLOWS_TENANT="default"
export RUST_LOG="info,agent_nexus=debug,openflows_harness=debug"
```

## Useful Test Scenarios

### Scenario 1: Normal Flow (Happy Path)
1. Reset state
2. Create a simple issue ("Add a hello.txt file with 'Hello World'")
3. Watch it progress through phases: planning → building → testing → review_ready
4. Verify PR is created and merged

### Scenario 2: Recovery from Workspace Crash
1. Create an issue
2. Let it start provisioning
3. Manually crash the workspace: `coder delete <workspace-name>`
4. Controller should detect crash (heartbeat missing)
5. Workspace should be recreated automatically
6. Work should resume

### Scenario 3: Exhausted Recovery (Escalation)
1. Create an issue
2. Force-delete the workspace 3 times
3. Ticket should escalate to `AwaitingHuman`
4. Worker should return to `Idle`
5. Next issue should be picked up

### Scenario 4: Re-provisioning Retry
1. Manually set a worker to `Assigned` with `workspace_id=None`:
   ```bash
   docker exec openflows-redis-1 redis-cli <<EOF
   GET worker_slots | jq '."forge-1".workspace_id = null' | \
   docker exec -i openflows-redis-1 redis-cli SET worker_slots
   EOF
   ```
2. Next poll should detect and retry provisioning
3. Workspace should be created within 3 attempts or escalate

## Monitoring in Production

For long-running tests, monitor key metrics:

```bash
# Watch worker turnover
watch -n 5 'docker exec openflows-redis-1 redis-cli GET "worker_slots" | jq ".[] | select(.status != \"Idle\")"'

# Count active tickets
watch -n 5 'docker exec openflows-redis-1 redis-cli GET "tickets" | jq "length"'

# Track recovery attempts (should be 0 after healthy recovery)
watch -n 5 'docker exec openflows-redis-1 redis-cli KEYS "ticket:*:recovery_attempts" | xargs docker exec -i openflows-redis-1 redis-cli MGET'

# Monitor heartbeats (should exist for all Assigned/Working slots)
watch -n 5 'docker exec openflows-redis-1 redis-cli KEYS "heartbeat:*"'
```
