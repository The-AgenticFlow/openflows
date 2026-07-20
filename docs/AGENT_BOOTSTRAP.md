# Agent Bootstrap: SessionStart Hook as the Entrypoint

## Problem: Hardcoded Prompts vs. Hook-Driven Bootstrap

### The Wrong Way (Previously)
```
Controller sends hardcoded chat message:
  "Work on ticket T-001. Review the dispatch payload and begin implementation."

Agent sees:
  ❌ Generic instructions (doesn't know what to do)
  ❌ Assumes dispatch is available (but doesn't know how to access it)
  ❌ No guidance on using the harness for coordination
  ❌ No current phase context (resuming work loses history)
  ❌ Persona and role context is missing
```

### The Right Way (Now)
```
Controller creates chat with NO initial message.
Workspace starts and fires SessionStart hook.
Hook outputs comprehensive bootstrap context to stdout.
Claude Code uses hook stdout as session context.

Agent sees:
  ✅ Full task dispatch payload
  ✅ Current phase (planning, building, testing, etc.)
  ✅ Harness commands reference (how to coordinate)
  ✅ Workflow steps and decision tree
  ✅ Example commands for each phase
  ✅ Policy constraints (use harness, no direct Redis, etc.)
```

## How It Works

### 1. Workspace Provisioning (agent-nexus post)

```rust
// Create chat with EMPTY content vector
let chat_req = CreateChatRequest {
    organization_id,
    workspace_id: workspace_id.clone(),
    model_config_id: None,
    content: vec![],  // NO initial message
    labels: Some(labels),
};
```

**Why?** Empty chat allows the hook to be the sole entrypoint. The hook output becomes the session context automatically.

### 2. Workspace Startup (Terraform startup_script)

```bash
# Harness is installed (required — exits on failure)
/usr/local/bin/openflows-harness

# Hooks are provisioned from the orchestration volume
cp -r /home/coder/.openflows/orchestration/plugin/hooks/forge/. ~/.openflows/hooks/

# Claude Settings are wired with hook event mappings
python3 <<EOF
# Maps SessionStart, PreToolUse, PostToolUse, Stop, etc.
# to actual hook scripts
EOF
```

### 3. Claude Code Connects (Agent CLI startup)

```
Claude Code starts.
Reads workspace environment (OPENFLOWS_TICKET, OPENFLOWS_ROLE, etc.).
Fires SessionStart hook.
Hook runs: ~/.openflows/hooks/session_start.sh
Hook stdout → Session context
```

### 4. SessionStart Hook Fires

`orchestration/plugin/hooks/forge/session_start.sh`:

```bash
echo "=== OpenFlows Forge Session ==="
echo "Ticket: T-001, Role: forge"
echo ""

# Read the actual task
openflows-harness dispatch read

# Show current phase
phase=$(openflows-harness status get | sed -n 's/.*"phase":"\([^"]*\)".*/\1/p')
echo "Current Phase: $phase"

# Explain the workflow and commands
echo "Workflow: planning → building → testing → review_ready"
echo "Commands: status set <phase>, pr opened, handoff write, etc."
```

**Hook output becomes the session context** — agent's first view of the world.

### 5. Agent Works with Harness

Agent is now informed and coordinated:

```bash
# Agent reads current task
$ openflows-harness dispatch read
{
  "ticket_id": "T-001",
  "title": "Add hello.txt",
  "body": "Create a file with 'Hello World'"
}

# Agent starts building
$ openflows-harness status set building
# ...implement...

# When done with implementation
$ openflows-harness status set testing
# ...run tests...

# Open PR and record it
$ git push origin forge-t-001
$ openflows-harness pr opened --pr 42 --branch forge-t-001 --title "Add hello.txt"

# Write handoff for sentinel
$ cat > HANDOFF.md <<EOF
## Changes
- Added hello.txt with "Hello World"

## Testing
- File created: ✅
- Content verified: ✅
EOF
$ openflows-harness handoff write --contract HANDOFF.md

# Move to review phase
$ openflows-harness status set review_ready
```

## Hook System Integration

### Events and Handlers

| Event | Hook | Purpose | Blocks? |
|-------|------|---------|---------|
| `SessionStart` | `session_start.sh` | Bootstrap context | No (output → context) |
| `PreToolUse` | `pre_bash_guard.sh` | Block destructive bash | Yes (exit 2) |
| `PreToolUse` | `pre_write_check.sh` | Prevent writes outside workspace | Yes (exit 2) |
| `PostToolUse` | `post_write_lint.sh` | Lint after edits | No (informational) |
| `PreCompact` | `pre_compact_handoff.sh` | Persist state before compaction | No |
| `Stop` | `stop_require_artifact.sh` | Block stop until artifacts exist | Yes (exit 2) |
| `SubagentStop` | `subagent_stop.sh` | Cleanup on subagent exit | No |

### How SessionStart Context Works

In Claude Code, hook script output becomes session context:

```
SessionStart hook stdout:
  "=== OpenFlows Forge Session ===
   Ticket: T-001
   Phase: planning
   Workflow: planning → building → testing → review_ready
   Commands: openflows-harness status set building, ..."

Claude Code sees this as system context:
  → Agent knows the ticket, phase, workflow, and available commands
  → Agent can follow the workflow naturally
  → No hardcoded prompt needed
```

## Why This Design

### 1. **Accurate Context**
The hook reads live state from Redis (dispatch, phase, heartbeat) instead of relying on stale controller data.

### 2. **Resilience**
If a session is interrupted and resumed, the hook re-reads the current phase and reflects where work actually left off.

### 3. **Loose Coupling**
The agent doesn't know about Coder or Redis internals — it only knows the harness interface (which is documented in the hook).

### 4. **Persona-Driven**
The hook can be customized per role:
- **forge**: Implementation workflow (planning → building → testing → review_ready)
- **sentinel**: Review workflow (read PR → evaluate → verdict)
- **vessel**: Merge workflow (squash → test → merge)

### 5. **Policy Enforcement**
The `PreToolUse`, `PreWrite`, and `Stop` hooks enforce constraints before the agent even tries dangerous actions.

## Comparison: Old vs. New Bootstrap

### Old (Hardcoded Prompt)
```
Controller:
  "Work on ticket T-001. Review dispatch and begin implementation."

Agent sees:
  - Generic task instruction (doesn't know what the task IS)
  - Assumes dispatch is accessible (but where? how?)
  - No phase context (can't resume work)
  - No command reference (has to guess)
  - No constraints (might delete files or force-push)
  - No handoff contract format (doesn't know what to produce)

Result: Agent fumbles, needs multiple clarifications, wastes turns.
```

### New (Hook-Driven Context)
```
SessionStart hook:
  "=== OpenFlows Forge Session ===
   Ticket: T-001, Phase: planning
   
   Dispatch:
   {title: "Add hello.txt", body: "Create with 'Hello World'"}
   
   Workflow: planning → building → testing → review_ready
   
   Commands:
     openflows-harness status set <phase>
     openflows-harness pr opened --pr N --branch B --title T
     openflows-harness handoff write --contract FILE
   
   Example flow:
     1. openflows-harness status set building
     2. [implement]
     3. openflows-harness pr opened --pr 42 --branch t-001 --title "Add hello.txt"
     4. openflows-harness status set review_ready"

Agent sees:
  - Exact task (title + body in dispatch)
  - Current phase (can resume from here)
  - Available commands (knows how to coordinate)
  - Example workflow (understands next steps)
  - Handoff requirement (knows what to produce)

Result: Agent starts work immediately, follows workflow, coordinates seamlessly.
```

## Implementation Checklist

- ✅ Remove hardcoded prompt from `create_chat_for_assignment` (agent-nexus)
- ✅ Create chats with empty content vector
- ✅ Implement comprehensive `session_start.sh` hook
- ✅ Hook reads dispatch, phase, and provides examples
- ✅ Terraform installs hooks and wires Claude settings
- ✅ Make harness install mandatory (fail startup on error)
- ✅ Follow-up prompts (resuming work) also use harness context

## Testing the Flow

```bash
# 1. Reset state
./scripts/reset-controller-state.sh --confirm

# 2. Start controller
cargo run -p openflows --bin agentflow &

# 3. Create test issue
# (GitHub web UI)

# 4. Watch workspace provision
tail -f /tmp/openflows-controller.log | grep "Provisioning\|provisioned"

# 5. SSH into workspace and check hook output
coder ssh <workspace> -- bash ~/.openflows/hooks/session_start.sh

# 6. Check dispatch is readable
coder ssh <workspace> -- openflows-harness dispatch read

# 7. Agent session starts — hook context is already there
# (Claude Code connects → SessionStart fires → hook context injected)
```

## Future: Persona-Driven Bootstraps

Once agent personalities/personas are integrated, each role can have custom bootstrap logic:

```bash
# forge/session_start.sh
echo "You are the Builder. Your job is to..."

# sentinel/session_start.sh
echo "You are the Reviewer. Your job is to..."

# vessel/session_start.sh
echo "You are the Merger. Your job is to..."

# lore/session_start.sh
echo "You are the Documentarian. Your job is to..."
```

The hook becomes a true entry point that embodies the role's persona and guides the agent through its responsibilities.
