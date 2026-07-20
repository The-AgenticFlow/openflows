#!/bin/bash
# Stop hook — refuse to end the session until the flow artifacts exist.
# Exit 2 blocks the stop and feeds stderr back to the agent, which keeps the
# forge loop moving through the defined flow instead of ending silently.

input=$(cat)

# Avoid infinite stop loops: if the agent is already continuing because of a
# previous stop-hook block, let it stop.
case "$input" in
  *'"stop_hook_active":true'* | *'"stop_hook_active": true'*) exit 0 ;;
esac

if ! command -v openflows-harness >/dev/null 2>&1; then
  exit 0
fi

pr=$(openflows-harness pr get 2>/dev/null || echo '{}')
case "$pr" in
  *'"pr_number"'*) exit 0 ;;
esac

status=$(openflows-harness status get 2>/dev/null || echo '{}')
phase=$(printf '%s' "$status" | sed -n 's/.*"phase"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')

case "$phase" in
  review_ready|blocked)
    # Terminal-enough states: sentinel/human takes over from here.
    exit 0
    ;;
  *)
    {
      echo "Ticket ${OPENFLOWS_TICKET:-?} is not finished (phase='${phase:-unset}', no PR recorded)."
      echo "Complete the flow before stopping:"
      echo "  1. Finish implementation and tests, updating 'openflows-harness status set building|testing'."
      echo "  2. Open the PR and record it: openflows-harness pr opened --pr <n> --branch <branch> --title <title>."
      echo "  3. Write the handoff: openflows-harness handoff write --contract <file>."
      echo "  4. Mark review readiness: openflows-harness status set review_ready."
      echo "If you are genuinely stuck, run: openflows-harness status set blocked (and explain why)."
    } >&2
    exit 2
    ;;
esac
