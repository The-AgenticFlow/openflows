#!/bin/bash
# PreToolUse(Bash) hook — block destructive or out-of-policy commands.
# Exit 2 blocks the tool call and returns stderr to the agent.

input=$(cat)
cmd=$(printf '%s' "$input" | python3 -c 'import json,sys
try:
    print(json.load(sys.stdin).get("tool_input", {}).get("command", ""))
except Exception:
    pass' 2>/dev/null)

[ -z "$cmd" ] && exit 0

deny() { echo "Blocked by forge policy: $1" >&2; exit 2; }

case "$cmd" in
  *"rm -rf /"*|*"rm -rf /*"*) deny "recursive delete of filesystem root" ;;
  *"git push"*"--force"*main*|*"git push"*"-f"*main*) deny "force-push to main" ;;
  *"git push"*"--force"*master*|*"git push"*"-f"*master*) deny "force-push to master" ;;
  *"redis-cli"*) deny "direct Redis access — use openflows-harness for all coordination" ;;
esac

# Workspace/template lifecycle is controller-managed. Workers must NEVER
# provision, start, stop, delete, recreate, or view-manipulate Coder
# workspaces or templates for their task — NEXUS owns that and binds the
# chat to the already-provisioned workspace. Spawning a fresh workspace
# here creates a duplicate/parallel workspace that breaks orchestration.
# Matching is anchored to an actual `coder` CLI invocation instead of an
# unanchored substring, so benign text that merely mentions "coder start"
# (docs, diagnostics) is not denied.
if printf '%s\n' "$cmd" | grep -Eq '(^|[;&|()[:space:]])([^[:space:];&|()]*/)?coder([[:space:]]+-[^[:space:]]+)*[[:space:]]+(templates|template|delete|create|start|stop|workspace|workspaces|server)([[:space:]]|$)'; then
  deny "workspace/template lifecycle is controller-managed — NEXUS provisions workspaces, never self-provision from a worker"
fi
exit 0
