#!/bin/bash
# SENTINEL Subagent Start Hook
# Runs when SENTINEL spawns a subagent for evaluation tasks
#
# Environment:
#   SPRINTLESS_PAIR_ID - the pair identifier
#   SPRINTLESS_TICKET_ID - the ticket being worked on
#   SPRINTLESS_SHARED - the shared directory
#   SPRINTLESS_SEGMENT - segment number or "final"
#   CODEX_SUBAGENT_ID - unique identifier for this subagent (if available)
#   CODEX_SUBAGENT_TASK - task description for the subagent (if available)

PAIR_ID="${SPRINTLESS_PAIR_ID}"
TICKET_ID="${SPRINTLESS_TICKET_ID}"
SHARED="${SPRINTLESS_SHARED}"
SEGMENT="${SPRINTLESS_SEGMENT}"
SUBAGENT_ID="${CODEX_SUBAGENT_ID:-unknown}"
SUBAGENT_TASK="${CODEX_SUBAGENT_TASK:-unspecified}"

# Log subagent spawn to shared event log
echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] sentinel-${PAIR_ID} subagent_start id=${SUBAGENT_ID} segment=${SEGMENT} task=${SUBAGENT_TASK}" \
  >> "${SHARED}/../events.log"

echo "=========================================="
echo "  SENTINEL SUBAGENT STARTED"
echo "=========================================="
echo ""
echo "Subagent ID: ${SUBAGENT_ID}"
echo "Segment: ${SEGMENT}"
echo "Task: ${SUBAGENT_TASK}"
echo "Pair: ${PAIR_ID}"
echo "Ticket: ${TICKET_ID}"
echo ""
echo "IMPORTANT - Directory Structure:"
echo "  SHARED: ${SHARED}"
echo "    -> Read PLAN.md, CONTRACT.md, WORKLOG.md for context"
echo "    -> Write evaluation results here"
echo ""
if [ -z "${SEGMENT}" ]; then
  echo "Subagent Guidelines (PlanReview):"
  echo "  1. Read the plan carefully"
  echo "  2. Check it has required sections (Understanding, Segments, Files Changed, Risks)"
  echo "  3. Write CONTRACT.md to ${SHARED}/CONTRACT.md"
  echo "  4. Include status: AGREED or ISSUES"
  echo "  5. Exit with code 0 on success, non-zero on failure"
elif [ "${SEGMENT}" = "final" ]; then
  echo "Subagent Guidelines (FinalReview):"
  echo "  1. Read the evaluation task carefully"
  echo "  2. Review the implementation against PLAN.md"
  echo "  3. Run tests and linters as specified"
  echo "  4. Write evaluation to final-review.md"
  echo "  5. Include verdict: APPROVED or NEEDS_WORK"
  echo "  6. Exit with code 0 on success, non-zero on failure"
else
  echo "Subagent Guidelines (SegmentEval):"
  echo "  1. Read the evaluation task carefully"
  echo "  2. Review the segment implementation against PLAN.md"
  echo "  3. Run tests and linters as specified"
  echo "  4. Write evaluation to segment-${SEGMENT}-eval.md"
  echo "  5. Include verdict: APPROVED or NEEDS_WORK"
  echo "  6. Exit with code 0 on success, non-zero on failure"
fi
echo ""

exit 0