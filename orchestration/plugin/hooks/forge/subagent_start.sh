#!/bin/bash
# FORGE Subagent Start Hook
# Runs when FORGE spawns a subagent to delegate work
#
# Environment:
#   SPRINTLESS_PAIR_ID - the pair identifier
#   SPRINTLESS_TICKET_ID - the ticket being worked on
#   SPRINTLESS_WORKTREE - the worktree directory
#   SPRINTLESS_SHARED - the shared directory
#   CODEX_SUBAGENT_ID - unique identifier for this subagent (if available)
#   CODEX_SUBAGENT_TASK - task description for the subagent (if available)

PAIR_ID="${SPRINTLESS_PAIR_ID}"
TICKET_ID="${SPRINTLESS_TICKET_ID}"
WORKTREE="${SPRINTLESS_WORKTREE}"
SHARED="${SPRINTLESS_SHARED}"
SUBAGENT_ID="${CODEX_SUBAGENT_ID:-unknown}"
SUBAGENT_TASK="${CODEX_SUBAGENT_TASK:-unspecified}"

# Log subagent spawn to shared event log
echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] forge-${PAIR_ID} subagent_start id=${SUBAGENT_ID} task=${SUBAGENT_TASK}" \
  >> "${SHARED}/../events.log"

echo "=========================================="
echo "  FORGE SUBAGENT STARTED"
echo "=========================================="
echo ""
echo "Subagent ID: ${SUBAGENT_ID}"
echo "Task: ${SUBAGENT_TASK}"
echo "Pair: ${PAIR_ID}"
echo "Ticket: ${TICKET_ID}"
echo ""
echo "IMPORTANT - Directory Structure:"
echo "  WORKTREE: ${WORKTREE}"
echo "    -> Read/write source code here"
echo "  SHARED: ${SHARED}"
echo "    -> Read PLAN.md, CONTRACT.md, TICKET.md for context"
echo ""
echo "Subagent Guidelines:"
echo "  1. Read the task description carefully"
echo "  2. Check PLAN.md for segment context (if applicable)"
echo "  3. Implement only the assigned task"
echo "  4. Write results to the worktree directory"
echo "  5. Update WORKLOG.md with your progress"
echo "  6. Exit with code 0 on success, non-zero on failure"
echo ""

exit 0