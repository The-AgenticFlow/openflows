#!/bin/bash
# PreCompact hook — persist a progress snapshot before context compaction so
# work state survives in the SharedStore even if in-context details are lost.

if command -v openflows-harness >/dev/null 2>&1; then
  status=$(openflows-harness status get 2>/dev/null || echo '{}')
  phase=$(printf '%s' "$status" | sed -n 's/.*"phase"[[:space:]]*:[[:space:]]*"\([^"]*\)".*/\1/p')
  # Re-assert the current phase (refreshes the timestamp) so the controller
  # sees recent activity across the compaction boundary.
  [ -n "$phase" ] && openflows-harness status set "$phase" >/dev/null 2>&1 || true
fi
echo "Context is being compacted. Re-read the dispatch with 'openflows-harness dispatch read' if needed."
exit 0
