#!/bin/bash
# === OpenFlows Forge Session Bootstrap ===
#
# This hook is the SOLE entrypoint for the agent session. It runs before the
# agent sees any chat context and provides:
#   1. Workspace environment verification
#   2. Task dispatch payload (what to work on)
#   3. Current phase and history (resume from where work left off)
#   4. Harness commands reference (how to coordinate)
#   5. Role persona and next actions
#
# All output here becomes the session context for the Claude Code agent.

set -e

# Colors for readability
BOLD="\033[1m"
CYAN="\033[36m"
GREEN="\033[32m"
YELLOW="\033[33m"
NC="\033[0m"  # No color

echo -e "${BOLD}${CYAN}=== OpenFlows Forge Session ===${NC}"
echo ""

# Environment check
if [ -z "$OPENFLOWS_TICKET" ] || [ -z "$OPENFLOWS_ROLE" ]; then
    echo -e "${YELLOW}⚠ Environment not fully configured${NC}"
    echo "  OPENFLOWS_TICKET=$OPENFLOWS_TICKET"
    echo "  OPENFLOWS_ROLE=$OPENFLOWS_ROLE"
    echo "  (This is expected if running outside a provisioned workspace.)"
    exit 0
fi

echo -e "${BOLD}Assignment:${NC}"
echo "  Ticket: ${CYAN}$OPENFLOWS_TICKET${NC}"
echo "  Role: ${CYAN}${OPENFLOWS_ROLE}${NC}"
echo ""

# Harness verification
if ! command -v openflows-harness >/dev/null 2>&1; then
    echo -e "${YELLOW}⚠ openflows-harness not found in PATH${NC}"
    echo "  Coordination with the controller is unavailable."
    echo "  Install: /usr/local/bin/openflows-harness"
    exit 0
fi

echo -e "${BOLD}Task Dispatch:${NC}"
if dispatch=$(openflows-harness dispatch read 2>/dev/null); then
    echo "$dispatch" | jq . 2>/dev/null || echo "$dispatch"
else
    echo "  (No dispatch payload yet — controller may still be processing.)"
fi
echo ""

# Current phase
phase_json=$(openflows-harness status get 2>/dev/null || echo '{}')
phase=$(printf '%s' "$phase_json" | sed -n 's/.*"phase"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
timestamp=$(printf '%s' "$phase_json" | sed -n 's/.*"ts"[[:space:]]*:[[:space:]]*\([0-9]*\).*/\1/p')

if [ -n "$phase" ]; then
    echo -e "${BOLD}Current Phase:${NC} ${GREEN}$phase${NC}"
    if [ -n "$timestamp" ]; then
        when=$(date -d "@$timestamp" "+%Y-%m-%d %H:%M:%S" 2>/dev/null || echo "(timestamp: $timestamp)")
        echo "  Set at: $when"
    fi
    echo -e "  ${YELLOW}Resume from this phase. You may have made progress here already.${NC}"
else
    echo -e "${BOLD}Current Phase:${NC} ${GREEN}planning${NC} (initial)"
    echo "  This is a fresh assignment. Follow the workflow below."
fi
echo ""

echo -e "${BOLD}Workflow:${NC}"
cat <<'EOF'
  1. planning  → Review the task and plan the approach
  2. building  → Implement the solution
  3. testing   → Run tests and verify the solution works
  4. review_ready → PR is open and ready for review
  5. blocked   → Stuck? Use this to pause and explain

Run at each phase:
  openflows-harness status set <phase>

Example flow:
  $ # Read the dispatch to understand the task
  $ openflows-harness dispatch read

  $ # Start building
  $ openflows-harness status set building
  $ # ...implement...

  $ # Open a PR when ready
  $ git push origin <branch>
  $ # Create PR on GitHub, get the PR number
  
  $ # Record the PR
  $ openflows-harness pr opened --pr <number> --branch <branch> --title "<title>"

  $ # Move to review phase
  $ openflows-harness status set review_ready

  $ # Prepare handoff contract (markdown summary of changes)
  $ openflows-harness handoff write --contract changes.md --notes "Ready for sentinel review"
EOF
echo ""

echo -e "${BOLD}Harness Commands:${NC}"
cat <<'EOF'
Coordination:
  openflows-harness dispatch read              # Read the task payload
  openflows-harness status set <phase>         # Update your progress
  openflows-harness status get                 # Check current phase
  openflows-harness pr opened --pr N --branch B --title "Title"  # Record PR
  openflows-harness handoff write --contract F # Hand off to sentinel

Policy:
  - All coordination MUST go through the harness (no direct Redis)
  - Phase changes are tracked in Redis and visible to the controller
  - Blocked state is for unresolvable issues (explain in the reason)
  - Heartbeat is automatic — it confirms the workspace is alive
EOF
echo ""

echo -e "${BOLD}${GREEN}Ready to work.${NC} Start with: ${CYAN}openflows-harness dispatch read${NC}"
echo ""
