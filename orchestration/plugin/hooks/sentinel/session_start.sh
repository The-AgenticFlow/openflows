#!/bin/bash
# SENTINEL Session Start Hook
# Determines mode and reads the correct input artifacts
#
# Environment:
#   SPRINTLESS_SHARED - the shared directory
#   SPRINTLESS_SEGMENT - segment number (empty for PlanReview, "final" for FinalReview, N for SegmentEval)

SHARED="${SPRINTLESS_SHARED}"
SEGMENT="${SPRINTLESS_SEGMENT}"

echo "=============================================="
echo "  SENTINEL SESSION STARTED"
echo "=============================================="
echo ""

# Determine mode based on SPRINTLESS_SEGMENT
if [ -z "${SEGMENT}" ]; then
  # PlanReview mode: SPRINTLESS_SEGMENT is empty
  echo "Mode: PLAN_REVIEW"
  echo "Segment: (plan review - no segment)"
  echo ""
  echo "Reading PLAN.md for review..."
  echo ""
  if [ -f "${SHARED}/PLAN.md" ]; then
    echo "--- PLAN.md (first 50 lines) ---"
    head -50 "${SHARED}/PLAN.md"
    echo "..."
  else
    echo "WARNING: No PLAN.md found yet."
  fi
  if [ -f "${SHARED}/TICKET.md" ]; then
    echo ""
    echo "--- TICKET.md ---"
    cat "${SHARED}/TICKET.md"
  fi
  echo ""
  echo "=============================================="
  echo "  YOUR MISSION"
  echo "=============================================="
  echo ""
  echo "1. Read PLAN.md carefully"
  echo "2. Check it has: ## Understanding, ## Segments, ## Files Changed, ## Risks"
  echo "3. REJECT generic/placeholder content or segments without file lists"
  echo "4. Write CONTRACT.md to ${SHARED}/CONTRACT.md"
  echo ""
  echo "CONTRACT.md format:"
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
  echo ""
elif [ "${SEGMENT}" = "final" ] || [ -f "${SHARED}/DONE.md" ]; then
  echo "Mode: FINAL_REVIEW"
  echo "Segment: final"
  echo ""
  echo "Reading DONE.md to verify completion..."
  if [ -f "${SHARED}/DONE.md" ]; then
    echo "--- DONE.md ---"
    head -50 "${SHARED}/DONE.md"
    echo "..."
  else
    echo "ERROR: DONE.md not found. Cannot perform final review."
    exit 1
  fi
  echo ""
  echo "=============================================="
  echo "  YOUR MISSION"
  echo "=============================================="
  echo ""
  echo "1. Read the segment changes from WORKLOG.md"
  echo "2. Run tests and linters to verify quality"
  echo "3. Write your evaluation to ${SHARED}/final-review.md"
  echo ""
  echo "Evaluation must include:"
  echo "  - ## Summary"
  echo "  - ## Tests Run"
  echo "  - ## Issues Found (if any)"
  echo "  - ## Verdict (APPROVED / NEEDS_WORK)"
  echo ""
  echo "If NEEDS_WORK, list specific issues that must be fixed."
  echo ""
else
  echo "Mode: SEGMENT_REVIEW"
  echo "Segment: ${SEGMENT}"
  echo ""
  echo "Reading PLAN.md and segment inputs..."
  echo ""
  if [ -f "${SHARED}/PLAN.md" ]; then
    echo "--- PLAN.md (first 30 lines) ---"
    head -30 "${SHARED}/PLAN.md"
    echo "..."
  fi
  echo ""
  if [ -f "${SHARED}/WORKLOG.md" ]; then
    echo "--- WORKLOG.md (last 20 lines) ---"
    tail -20 "${SHARED}/WORKLOG.md"
    echo ""
  fi
  echo ""
  echo "=============================================="
  echo "  YOUR MISSION"
  echo "=============================================="
  echo ""
  echo "1. Read the segment changes from WORKLOG.md"
  echo "2. Run tests and linters to verify quality"
  echo "3. Write your evaluation to ${SHARED}/segment-${SEGMENT}-eval.md"
  echo ""
  echo "Evaluation must include:"
  echo "  - ## Summary"
  echo "  - ## Tests Run"
  echo "  - ## Issues Found (if any)"
  echo "  - ## Verdict (APPROVED / NEEDS_WORK)"
  echo ""
  echo "If NEEDS_WORK, list specific issues that must be fixed."
  echo ""
fi

exit 0