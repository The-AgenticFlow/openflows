#!/bin/bash
# SENTINEL Subagent Stop Hook
# Runs when a SENTINEL subagent completes or fails
#
# Environment:
#   SPRINTLESS_PAIR_ID - the pair identifier
#   SPRINTLESS_TICKET_ID - the ticket being worked on
#   SPRINTLESS_SHARED - the shared directory
#   SPRINTLESS_SEGMENT - segment number or "final"
#   CODEX_SUBAGENT_ID - unique identifier for this subagent (if available)
#   CODEX_SUBAGENT_EXIT_CODE - exit code from the subagent (if available)
#   CODEX_SUBAGENT_STATUS - success/failure/timeout (if available)

PAIR_ID="${SPRINTLESS_PAIR_ID}"
TICKET_ID="${SPRINTLESS_TICKET_ID}"
SHARED="${SPRINTLESS_SHARED}"
SEGMENT="${SPRINTLESS_SEGMENT}"
SUBAGENT_ID="${CODEX_SUBAGENT_ID:-unknown}"
EXIT_CODE="${CODEX_SUBAGENT_EXIT_CODE:-unknown}"
STATUS="${CODEX_SUBAGENT_STATUS:-unknown}"

# Log subagent completion to shared event log
echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] sentinel-${PAIR_ID} subagent_stop id=${SUBAGENT_ID} segment=${SEGMENT} exit=${EXIT_CODE} status=${STATUS}" \
  >> "${SHARED}/../events.log"

echo "=========================================="
echo "  SENTINEL SUBAGENT COMPLETED"
echo "=========================================="
echo ""
echo "Subagent ID: ${SUBAGENT_ID}"
echo "Segment: ${SEGMENT}"
echo "Exit Code: ${EXIT_CODE}"
echo "Status: ${STATUS}"
echo ""

# Validate subagent output — determine expected artifact based on mode
if [ -z "${SEGMENT}" ]; then
  EVAL_FILE="CONTRACT.md"
elif [ "${SEGMENT}" = "final" ]; then
  EVAL_FILE="final-review.md"
else
  EVAL_FILE="segment-${SEGMENT}-eval.md"
fi

if [ "${EXIT_CODE}" = "0" ] || [ "${STATUS}" = "success" ]; then
  echo "Result: SUCCESS"
  echo "Subagent completed successfully."
  echo ""
  echo "Next steps:"
  echo "  1. Verify evaluation file exists: ${SHARED}/${EVAL_FILE}"
  echo "  2. Check evaluation verdict (APPROVED/NEEDS_WORK)"
  echo "  3. Signal harness with FsEvent::SegmentEvalWritten or FinalReviewWritten"
else
  echo "Result: FAILURE"
  echo "Subagent failed with exit code ${EXIT_CODE}."
  echo ""
  echo "Next steps:"
  echo "  1. Check subagent logs for error details"
  echo "  2. Retry evaluation if transient error"
  echo "  3. Mark as BLOCKED if unrecoverable"
fi

echo ""

exit 0