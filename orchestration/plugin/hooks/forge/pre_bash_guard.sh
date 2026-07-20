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
  *"coder templates"*|*"coder delete"*) deny "control-plane mutation from a worker workspace" ;;
esac
exit 0
