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

# ── Workspace identity & lifecycle ─────────────────────────────────────
# The Controller (NEXUS) has ALREADY provisioned and started this Coder
# workspace and bound the chat to it. The agent runs INSIDE that workspace;
# it must never provision, start, stop, delete, or recreate workspaces or
# pick templates — that is controller-managed and doing it here creates
# duplicate/parallel workspaces that break the ticket's orchestration.
if [ -n "$CODER_WORKSPACE_ID" ]; then
    echo -e "${BOLD}Your provisioned workspace:${NC} ${GREEN}$CODER_WORKSPACE_ID${NC}"
    echo -e "  You are ALREADY running inside this workspace. Do NOT create, start,"
    echo -e "  stop, delete, or re-provision any Coder workspace or template — NEXUS"
    echo -e "  owns the workspace lifecycle and has bound this chat to this workspace."
else
    echo -e "${YELLOW}⚠ No CODER_WORKSPACE_ID set — the controller may still be"
    echo -e "  provisioning this workspace. Do NOT create a new workspace or pick"
    echo -e "  a template; wait for the controller instead.${NC}"
fi
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
  planning -> plan_ready -> building -> testing -> submit -> done
  Plan rejection: plan_rejected -> planning -> revised plan -> plan_ready
  Test/PR/CI rejection: building -> testing -> submit

  1. Read dispatch and status get. Use absolute file paths under /home/coder/workspace.
     Verify the worker/ticket branch (e.g. forge-2/T-066), or the dispatched PR branch.
     Start/revise in planning.
  2. Write the plan at the current chat-specific path; run plan write --file <absolute-plan-path>; set plan_ready.
  3. Wait for SENTINEL approval, then set building and implement.
  4. Commit ALL work, set testing, and check the template-managed verify executor.
     Inspect /home/coder/.local/state/openflows/verify.log for infrastructure failures.
     Use blocked for those failures; do not cycle building/testing.
  5. Wait for successful A2A verification and SENTINEL testing approval, then set submit.
  6. Open/update PR; run pr opened --pr <N> --branch <branch> --title <title>.
  7. Wait for SENTINEL and HUMAN PR approval and CI success.

  Source is frozen during plan_ready, testing and submit. For corrections,
  return to building and repeat testing. For plan changes, return to planning.
  Use blocked for operational failure; recover through planning.
  Read revision/review_round/head from status get for every review decision.

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
