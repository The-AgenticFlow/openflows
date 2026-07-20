# Quick Start: Testing the Deadlock Fixes

## 1. Reset to Clean State

```bash
./scripts/reset-controller-state.sh --confirm
```

This clears:
- ✅ All tickets and ticket metadata (dispatch, status, chat, recovery_attempts)
- ✅ All worker slots (forge-1, forge-2, sentinel, vessel, lore)
- ✅ Pending PRs and command gate state
- ✅ All heartbeat records

Preserves:
- ✅ CI readiness state
- ✅ GitHub sync metadata (issues, PRs, branches)
- ✅ Agent registry (roles, CLI backends)

**Result**: Redis has only 6 keys left, `worker_slots` is gone, `tickets` is `[]`.

## 2. Verify Clean State

```bash
docker exec openflows-redis-1 redis-cli GET "worker_slots"  # Should be nil (missing)
docker exec openflows-redis-1 redis-cli GET "tickets"       # Should be []
docker exec openflows-redis-1 redis-cli DBSIZE              # Should be 6
```

## 3. Start the Controller

```bash
# In one terminal:
cargo run -p openflows --bin agentflow

# In another terminal, watch the logs:
tail -f /tmp/openflows-controller.log
```

## 4. Create a Test Issue in GitHub

```bash
# Create a simple issue to test the flow
# Title: "Test forge flow"
# Body: "Add a simple hello.txt with content"

# The controller will:
# 1. Sync issues (every 15s)
# 2. Create T-*** ticket in Redis
# 3. Assign to an idle forge worker (forge-1)
# 4. Provision a Coder workspace
# 5. Create a chat and inject the dispatch payload
```

## 5. Monitor Progress

```bash
# Watch for assignment
tail -f /tmp/openflows-controller.log | grep "Nexus:"

# Watch for provisioning
tail -f /tmp/openflows-controller.log | grep "Provisioning"

# Check worker slots after assignment
docker exec openflows-redis-1 redis-cli GET "worker_slots" | jq .

# Check ticket status
docker exec openflows-redis-1 redis-cli GET "ticket:T-001:status"

# Check heartbeat (should appear within 30s of workspace startup)
docker exec openflows-redis-1 redis-cli KEYS "heartbeat:*"
```

## 6. Test the Fixes

### Fix #1: Escalation frees the worker

To test: Force a workspace to crash and exceed recovery limit.

```bash
# In the Coder workspace terminal:
coder delete <workspace-name>

# Watch the controller:
# 1. Detects heartbeat missing
# 2. Attempts restart/recreate 3 times
# 3. Escalates to AwaitingHuman
# 4. **Key point**: Worker should return to Idle
docker exec openflows-redis-1 redis-cli GET "worker_slots" | jq '.["forge-1"].status'
# Should see: {"type": "idle"}

# Next issue should be picked up immediately (no "no idle forge worker — pausing" loops)
```

### Fix #2: Re-provisioning retry for workspace_id=None

To test: Manually create a busy-but-empty slot.

```bash
# Set a worker to Assigned with no workspace_id
docker exec openflows-redis-1 redis-cli SET worker_slots '
{
  "forge-1": {
    "id": "forge-1",
    "status": {"ticket_id": "T-999", "issue_url": "https://example.com", "type": "assigned"},
    "workspace_id": null
  }
}'

# On next controller poll, it should:
# 1. Detect workspace_id is null
# 2. Retry provisioning (attempt 1, 2, 3...)
# 3. Create the workspace on next successful attempt
# 4. Or escalate if it fails 3 times

tail -f /tmp/openflows-controller.log | grep -E "retry.*provisioning|retry limit"
```

### Fix #3: Harness provisioning and hooks

To test: Check that the harness is installed and wired into Claude settings.

```bash
# Inside the workspace:
which openflows-harness     # Should exist
openflows-harness --help    # Should show commands

# Check hooks are installed
ls -la ~/.openflows/hooks/

# Check settings.json is wired
cat ~/.claude/settings.json | jq .hooks

# Run a hook
~/.openflows/hooks/session_start.sh    # Should print dispatch context
```

## Expected Behavior (After Fixes)

### Happy Path (No Crashes)
```
Create issue → Assign to forge-1 → Provision workspace → Start heartbeat → 
Create chat → Dispatch payload → Agent implements → Open PR → 
Handoff to sentinel → Merge → Done
```

### Recovery Path (Workspace Crashes Once)
```
Workspace running → Heartbeat missing → Detect crash (attempt 1) → 
Recreate workspace → Restart heartbeat → Resume chat → Complete work
```

### Escalation Path (Workspace Crashes 3+ Times)
```
Workspace crash #1 → Recreate → Success → Continue
Workspace crash #2 → Recreate → Success → Continue
Workspace crash #3 → Recreate → Fails → Escalate to AwaitingHuman →
Worker returns to Idle → Next issue is picked up → Human fixes T-001 separately
```

## Troubleshooting

### "no idle forge worker — pausing" loops forever
- Worker is stuck in Assigned/Working state
- Run reset script to clean state
- Or manually free the worker:
  ```bash
  docker exec openflows-redis-1 redis-cli <<EOF
  GET worker_slots | jq '.["forge-1"].status = {"type": "idle"}' | \
  SET worker_slots
  EOF
  ```

### "Workspace not yet provisioning" (forge-2 stuck)
- Provisioning failed but assignment was kept
- Check error logs: `tail -f /tmp/openflows-controller.log | grep -i provision`
- Likely causes: CODER_URL/token missing, Coder template not found, quota exceeded
- Reset state and fix the underlying issue

### Heartbeat not appearing
- Harness may have failed to install or start
- Check workspace logs: `coder ssh <workspace> -- cat /tmp/startup.log`
- Verify: `coder ssh <workspace> -- which openflows-harness`
- If missing: manually run `openflows-harness heartbeat start`

### Hooks not firing
- Check settings.json was generated:
  ```bash
  coder ssh <workspace> -- cat ~/.claude/settings.json | jq .hooks
  ```
- Verify hooks are executable:
  ```bash
  coder ssh <workspace> -- ls -la ~/.openflows/hooks/
  ```
- Check Claude Code version supports hooks (v0.1.0+)

## Full Reset + Restart

```bash
# Complete clean slate
./scripts/reset-controller-state.sh --confirm

# Stop controller
pkill -f agentflow

# Delete all workspaces (optional but recommended)
coder list --offline | grep openflows | awk '{print $1}' | \
  xargs -I {} coder delete {} --force

# Start fresh
cargo run -p openflows --bin agentflow &
disown

# Monitor
tail -f /tmp/openflows-controller.log
```
