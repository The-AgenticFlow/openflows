# OpenFlows System Fixes: Complete Summary

## Problem Overview

The OpenFlows agent orchestration system had three critical issues preventing normal operation:

1. **Escalation Deadlock**: Crashed workspaces were escalated every 15 seconds without freeing worker slots, causing a permanent livelock with "no idle forge worker" messages
2. **Workspace Provisioning Gap**: Busy slots with `workspace_id=None` were invisible to recovery logic, leaving workers permanently stuck in Assigned state
3. **Generic Bootstrap**: Agents received hardcoded generic prompts instead of hook-driven context with real task dispatch and workflow guidance

---

## Fix 1: Escalation Frees Worker Slots & Resets Counter

**Files Changed**: `crates/agent-nexus/src/lib.rs`

### Problem
- When a workspace crashed after 3 recovery attempts, `mark_ticket_awaiting_human` would escalate the ticket
- But the worker slot stayed in `Assigned/Working` status
- Result: Nexus could never assign another ticket to that worker → permanent pause state

### Solution

**Added `release_worker_slot()` helper**:
```rust
async fn release_worker_slot(&self, store: &SharedStore, worker_id: &str) {
    // 1. Return worker to Idle
    slot.status = WorkerStatus::Idle;
    
    // 2. Destroy associated workspace (cleanup)
    destroy_coder_workspace(&workspace_id).await;
    
    // Log and save
    store.set(KEY_WORKER_SLOTS, json!(slots)).await;
}
```

**Updated `mark_ticket_awaiting_human()`**:
- Check if ticket is already `AwaitingHuman` → skip re-escalation (prevents notification loop)
- Reset recovery counter to 0 (human-triggered retry gets full 3-attempt budget)
- Call `release_worker_slot()` to return worker to Idle
- Only THEN notify

**Updated `inspect_coder_recovery()`**:
- When a workspace and heartbeat are healthy, reset recovery counter so later crashes get full budget

### Impact
- ✅ Worker is freed immediately when escalating
- ✅ Next poll, Nexus sees an idle worker and assigns the next ticket
- ✅ No more "no idle forge worker — pausing" loops
- ✅ Escalated ticket goes to human, other work continues

---

## Fix 2: Re-provisioning Retry for workspace_id=None

**Files Changed**: `crates/agent-nexus/src/lib.rs`

### Problem
- forge-2 was `Assigned` but had `workspace_id=None`
- Original provisioning failed silently (Coder creds missing or timeout)
- Recovery loop skipped it entirely (`let Some(workspace_id) = ... else { continue }`)
- Result: Slot never gets a workspace, chat never created, worker stuck forever

### Solution

**Added re-provisioning loop in `prep()`** (after recovery repair):
```rust
if slot.workspace_id.is_none() {
    // Bound by recovery counter: attempt 1, 2, 3...
    let attempts = increment_recovery_attempts(store, ticket_id).await;
    if attempts >= Ticket::MAX_ATTEMPTS {
        // Escalate like a crashed workspace
        mark_ticket_awaiting_human(store, ticket_id, worker_id, &reason).await;
        continue;
    }
    
    // Retry provisioning
    match provision_coder_workspace(store, worker_id, ticket_id).await {
        Ok(Some(_)) => reset_recovery_attempts(store, ticket_id).await,
        Ok(None) | Err(_) => continue,  // Will retry next poll
    }
}
```

### Impact
- ✅ Busy slots without workspaces are detected and provisioning is retried
- ✅ Bounded by 3 attempts (then escalates to human)
- ✅ Successful provisioning resets counter
- ✅ Socket is never left in indefinite "Assigned but no workspace" state

---

## Fix 3: SessionStart Hook as True Entrypoint (Remove Hardcoded Prompts)

**Files Changed**: 
- `crates/agent-nexus/src/lib.rs` (chat creation)
- `orchestration/plugin/hooks/forge/session_start.sh` (comprehensive bootstrap)
- `docs/AGENT_BOOTSTRAP.md` (design documentation)

### Problem
- Agents received generic hardcoded prompt: "Work on ticket X. Review dispatch and begin."
- This prompt didn't know what the actual task was, didn't mention harness commands, didn't show current phase
- Hook-driven context was ignored
- Resume (follow-up) messages were also generic

### Solution

**1. Create chats with NO initial message**:
```rust
let chat_req = CreateChatRequest {
    content: vec![],  // Empty — SessionStart hook is the entrypoint
    // ...other fields...
};
```

**2. SessionStart hook becomes the real bootstrap** (comprehensive context):
```bash
#!/bin/bash
# Print to stdout → becomes session context in Claude Code

echo "=== OpenFlows Forge Session ==="
echo "Ticket: $OPENFLOWS_TICKET, Role: $OPENFLOWS_ROLE"
echo ""

# Real task payload from Redis
openflows-harness dispatch read

# Current phase (supports resuming work)
phase=$(openflows-harness status get | jq -r '.phase')
echo "Current Phase: $phase"

# Workflow explanation with examples
echo "Workflow: planning → building → testing → review_ready → (sentinel reviews)"
echo ""
echo "Commands:"
echo "  openflows-harness status set <phase>"
echo "  openflows-harness pr opened --pr N --branch B --title T"
echo "  openflows-harness handoff write --contract FILE"
echo ""
echo "Example: openflows-harness dispatch read  # See the task"
```

**3. Harness-aware follow-up prompts** (for resumed work):
```rust
let follow_up_prompt = format!(
    "Resume work on ticket {}. Check your phase with \
     `openflows-harness status get` and dispatch with \
     `openflows-harness dispatch read`. Continue from there.",
    ticket_id
);
```

### Impact
- ✅ Agent starts with REAL task context (title, description, requirements)
- ✅ Agent knows current phase (can resume from interruptions)
- ✅ Agent has harness command reference built-in
- ✅ Agent understands workflow and next steps
- ✅ No confusion from generic prompts
- ✅ Tight coupling with harness coordination

---

## Supporting Fixes: Harness & Template

### Harness: Added Status/PR Read Commands

**Files Changed**: `crates/openflows-harness/src/main.rs`, `store.rs`

```rust
enum StatusAction {
    Set { phase: String },
    Get,  // NEW: read current phase
}

enum PrAction {
    Get,  // NEW: read recorded PR
    Opened { pr: u64, branch: String, title: String },
}
```

These commands allow hooks to read coordination state:
- `stop_require_artifact.sh` checks if PR was recorded before blocking stop
- `pre_compact_handoff.sh` re-asserts the current phase

### Template: Robust Harness Install + Hooks Provisioning

**Files Changed**: `crates/coder-client/templates/openflows-forge/main.tf`

```bash
# 1. Mandatory harness install with retries (exits startup on failure)
for attempt in 1 2 3; do
    curl -fsSL "$HARNESS_URL" -o /tmp/openflows-harness && break
    sleep 5
done
if [ ! -x /usr/local/bin/openflows-harness ]; then
    exit 1  # FATAL — workspace cannot coordinate
fi

# 2. Install hooks from orchestration volume
cp -r /home/coder/.openflows/orchestration/plugin/hooks/forge/. ~/.openflows/hooks/
chmod +x ~/.openflows/hooks/*.sh

# 3. Wire hooks into Claude settings
python3 <<EOF
# Generate ~/.claude/settings.json with:
# - SessionStart → session_start.sh
# - PreToolUse(Bash) → pre_bash_guard.sh
# - PreToolUse(Write) → pre_write_check.sh
# - PostToolUse(Write) → post_write_lint.sh
# - PreCompact → pre_compact_handoff.sh
# - Stop → stop_require_artifact.sh
# - SubagentStop → subagent_stop.sh
EOF

# 4. Fixed OPENFLOWS_ROLE to base role (forge, not forge-1)
export OPENFLOWS_ROLE="${replace(data.coder_parameter.role.value, "/-[0-9]+$/", "")}"
```

---

## Testing & Verification

### Reset Script: `./scripts/reset-controller-state.sh`

```bash
# Clean reset (removes 60+ zombie keys)
./scripts/reset-controller-state.sh --confirm

# Also delete Coder workspaces
./scripts/reset-controller-state.sh --full
```

Clears: tickets, worker_slots, PRs, heartbeats, recovery counters  
Preserves: CI readiness, GitHub sync metadata, agent registry

### Documentation

1. **`TESTING_QUICK_START.md`** — 6-step test workflow
2. **`docs/TESTING_GUIDE.md`** — Detailed guide with Redis key structure, debugging, scenarios
3. **`docs/AGENT_BOOTSTRAP.md`** — Design doc explaining hook-driven bootstrap
4. **`FIXES_SUMMARY.md`** (this file) — Complete fix overview

---

## Behavior Changes: Before vs. After

### Before
```
Workspace crashes → Escalate → Worker stays Assigned → No idle workers
                                                        ↓
                                        "no idle forge worker — pausing" forever
```

### After
```
Workspace crashes → Escalate → Worker freed to Idle → Next ticket assigned
                                                      ↓
                                        Work continues, human fixes T-001
```

### Before (Provisioning)
```
Provision fails silently → workspace_id = None → Chat creation fails
                                                   ↓
                          "Workspace not yet provisioning" every poll
                                                   ↓
                                              Stuck forever
```

### After (Provisioning)
```
Provision fails → Detect workspace_id=None → Retry (attempt 1, 2, 3)
                                              ↓ (succeeds)
                                        Workspace created → Chat → Work
                                              OR
                                        All attempts fail → Escalate
```

### Before (Bootstrap)
```
Agent: "Work on ticket T-001. Review dispatch and begin."
       ❌ Generic (doesn't know the task)
       ❌ No command reference
       ❌ No phase context
       ❌ No workflow steps
```

### After (Bootstrap)
```
Agent (from SessionStart hook): 
  "=== OpenFlows Forge Session ===
   Ticket: T-001, Phase: planning
   
   Dispatch: {title: 'Add hello.txt', body: '...'}
   
   Workflow: planning → building → testing → review_ready
   
   Commands:
     openflows-harness status set <phase>
     openflows-harness pr opened --pr N --branch B --title T
     ..."
   
   ✅ Knows the task
   ✅ Knows available commands
   ✅ Knows workflow
   ✅ Can resume from phase
```

---

## Files Summary

| File | Change | Impact |
|------|--------|--------|
| `agent-nexus/src/lib.rs` | Remove hardcoded prompt, add slot release, add re-provisioning, reset counter | Deadlock fix, bootstrap fix |
| `openflows-harness/src/main.rs` | Add `Status::Get`, `Pr::Get` | Hooks can read coordination state |
| `openflows-harness/src/store.rs` | Implement `status_get()`, `pr_get()` | Hook queries work |
| `coder-client/templates/main.tf` | Robust install, hooks provision, settings gen | Harness required, hooks wired |
| `hooks/forge/session_start.sh` | Real bootstrap context | Agent knows what to do |
| `hooks/forge/stop_require_artifact.sh` | Check phase/PR before allowing stop | Stop hook works |
| `hooks/forge/pre_bash_guard.sh` | Block dangerous commands | Policy enforced |
| `hooks/forge/pre_compact_handoff.sh` | Refresh phase timestamp | Handles compaction |
| `scripts/reset-controller-state.sh` | Automated clean reset | Testing enabled |
| `docs/AGENT_BOOTSTRAP.md` | Design doc | Understanding documented |
| `docs/TESTING_GUIDE.md` | Comprehensive testing guide | Testing enabled |
| `TESTING_QUICK_START.md` | Quick 6-step walkthrough | Getting started easy |

---

## Build & Test Status

✅ All crates build successfully  
✅ All unit tests pass (11 tests in agent-nexus + openflows-harness)  
✅ All hook scripts validate (bash -n)  
✅ No new clippy errors introduced  
✅ Full workspace builds with `cargo build`  

---

## Next Steps for Deployment

1. **Rebuild binaries**:
   ```bash
   cargo build --release
   ```

2. **Update deployed container** with new binary

3. **Reset any existing dead state**:
   ```bash
   ./scripts/reset-controller-state.sh --confirm
   ```

4. **Restart controller**:
   ```bash
   cargo run -p openflows --bin agentflow
   ```

5. **Monitor first ticket** (should provision, work, and complete without deadlock):
   ```bash
   tail -f /tmp/openflows-controller.log | grep -E "Nexus:|Provisioning|escalating"
   ```

---

## Expected Results

After these fixes, the system should:

1. ✅ Assign tickets to idle workers without deadlock
2. ✅ Provision workspaces reliably with retry and clear failure modes
3. ✅ Escalate unrecoverable crashes without blocking the fleet
4. ✅ Boot agents with real task context and harness command reference
5. ✅ Resume work from interrupted sessions with phase continuity
6. ✅ Complete the full workflow: assign → provision → work → handoff → review → merge
