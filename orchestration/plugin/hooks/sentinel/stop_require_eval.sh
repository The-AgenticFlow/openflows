#!/bin/bash
# SENTINEL Stop Hook
# Ensures SENTINEL writes an eval/review file before exiting
#
# Environment:
#   SPRINTLESS_SHARED - the shared directory
#   SPRINTLESS_SEGMENT - segment number (empty for PlanReview, "final" for FinalReview, N for SegmentEval)

SHARED="${SPRINTLESS_SHARED}"
SEGMENT="${SPRINTLESS_SEGMENT}"

# Determine mode and expected artifact
if [ -z "${SEGMENT}" ]; then
  # PlanReview mode: SPRINTLESS_SEGMENT is empty
  # SENTINEL must write CONTRACT.md with status AGREED or ISSUES
  MODE="PlanReview"
  EVAL_FILE="${SHARED}/CONTRACT.md"
  REQUIRED_STATUS_VALUES=("AGREED" "ISSUES")
else
  if [ "${SEGMENT}" = "final" ]; then
    MODE="FinalReview"
    EVAL_FILE="${SHARED}/final-review.md"
  else
    MODE="SegmentEval"
    EVAL_FILE="${SHARED}/segment-${SEGMENT}-eval.md"
  fi
  REQUIRED_STATUS_VALUES=("APPROVED" "NEEDS_WORK")
fi

# Check if the expected artifact exists
if [ -f "$EVAL_FILE" ]; then
  if [ "${MODE}" = "PlanReview" ]; then
    # Validate CONTRACT.md has a valid status line
    STATUS=$(grep -oP '^status:\s*\K[A-Z]+' "$EVAL_FILE" 2>/dev/null || echo "")
    if [ -z "$STATUS" ]; then
      # Also try matching "status: AGREED" or "status: ISSUES" with possible whitespace
      STATUS=$(grep -i "^status:" "$EVAL_FILE" | head -1 | sed 's/^status:[[:space:]]*//' | tr -d ' ')
    fi

    for valid in "${REQUIRED_STATUS_VALUES[@]}"; do
      if [ "$STATUS" = "$valid" ]; then
        echo "Plan review complete: ${STATUS}"
        exit 0
      fi
    done
    echo "ERROR: Invalid status in ${EVAL_FILE}"
    echo "Status must be AGREED or ISSUES, got: ${STATUS}"
    exit 2
  else
    # Validate segment/final eval files have a ## Verdict section
    if grep -q "## Verdict" "$EVAL_FILE"; then
      VERDICT=$(grep -A1 "## Verdict" "$EVAL_FILE" | tail -1 | tr -d ' ')

      for valid in "${REQUIRED_STATUS_VALUES[@]}"; do
        if [ "$VERDICT" = "$valid" ]; then
          echo "Evaluation complete: ${VERDICT}"
          exit 0
        fi
      done
      echo "ERROR: Invalid verdict in ${EVAL_FILE}"
      echo "Verdict must be APPROVED or NEEDS_WORK, got: ${VERDICT}"
      exit 2
    else
      echo "ERROR: ${EVAL_FILE} missing ## Verdict section"
      exit 2
    fi
  fi
fi

# No artifact file found - block exit
echo "=============================================="
echo "  BLOCKED: Cannot exit without evaluation"
echo "=============================================="
echo ""
echo "Mode: ${MODE}"
echo ""
echo "You must write your evaluation to:"
echo "  ${EVAL_FILE}"
echo ""

if [ "${MODE}" = "PlanReview" ]; then
  echo "Required format for CONTRACT.md:"
  echo "  ---"
  echo "  status: AGREED | ISSUES"
  echo "  summary: <one line>"
  echo "  definition_of_done:"
  echo "  - <criterion from plan>"
  echo "  objections:"
  echo "  - <specific issue or 'None'>"
  echo "  timeout_profile:"
  echo "    plan_review_secs: <number>"
  echo "    segment_eval_secs: <number>"
  echo "    final_review_secs: <number>"
  echo "    complexity: low | medium | high"
else
  echo "Required sections:"
  echo "  - ## Summary"
  echo "  - ## Tests Run"
  echo "  - ## Issues Found (if any)"
  echo "  - ## Verdict (APPROVED or NEEDS_WORK)"
  echo ""
  echo "If NEEDS_WORK, include:"
  echo "  - ## Required Fixes (specific, actionable items)"
fi

echo ""

exit 2