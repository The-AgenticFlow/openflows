#!/bin/bash
# FORGE Subagent Stop Hook
# Runs when a FORGE subagent completes or fails
#
# Environment:
#   SPRINTLESS_PAIR_ID - the pair identifier
#   SPRINTLESS_TICKET_ID - the ticket being worked on
#   SPRINTLESS_WORKTREE - the worktree directory
#   SPRINTLESS_SHARED - the shared directory
#   CODEX_SUBAGENT_ID - unique identifier for this subagent (if available)
#   CODEX_SUBAGENT_EXIT_CODE - exit code from the subagent (if available)
#   CODEX_SUBAGENT_STATUS - success/failure/timeout (if available)

PAIR_ID="${SPRINTLESS_PAIR_ID}"
TICKET_ID="${SPRINTLESS_TICKET_ID}"
WORKTREE="${SPRINTLESS_WORKTREE}"
SHARED="${SPRINTLESS_SHARED}"
SUBAGENT_ID="${CODEX_SUBAGENT_ID:-unknown}"
EXIT_CODE="${CODEX_SUBAGENT_EXIT_CODE:-unknown}"
STATUS="${CODEX_SUBAGENT_STATUS:-unknown}"

# Log subagent completion to shared event log
echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] forge-${PAIR_ID} subagent_stop id=${SUBAGENT_ID} exit=${EXIT_CODE} status=${STATUS}" \
  >> "${SHARED}/../events.log"

echo "=========================================="
echo "  FORGE SUBAGENT COMPLETED"
echo "=========================================="
echo ""
echo "Subagent ID: ${SUBAGENT_ID}"
echo "Exit Code: ${EXIT_CODE}"
echo "Status: ${STATUS}"
echo ""

# Validate subagent output
if [ "${EXIT_CODE}" = "0" ] || [ "${STATUS}" = "success" ]; then
  echo "Result: SUCCESS"
  echo "Subagent completed successfully."
  echo ""
  echo "Next steps:"
  echo "  1. Verify output files exist in worktree"
  echo "  2. Update WORKLOG.md with completion status"
  echo "  3. Continue with next segment or task"
else
  echo "Result: FAILURE"
  echo "Subagent failed with exit code ${EXIT_CODE}."
  echo ""
  echo "Next steps:"
  echo "  1. Check subagent logs for error details"
  echo "  2. Determine if retry is appropriate"
  echo "  3. Update STATUS.json with BLOCKED if unrecoverable"
fi

echo ""

exit 0