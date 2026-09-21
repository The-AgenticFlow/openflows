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
# Detection tokenizes the command and only flags `coder <subcommand>` at a
# command boundary (start, after a separator, or after `sudo`/a shell
# wrapper's `-c`), unwrapping quotes, so real invocations (including
# `bash -c 'coder start …'`) are blocked while benign text that merely
# mentions "coder start" (docs, diagnostics) is not denied.
_lc_denied() {
  local line="$1"
  local -a toks=() at=()
  local -a parts
  read -r -a parts <<< "$line"
  local part led tok at_start=1
  local re_lead='^[;&|(){}]' re_trail='[;&|(){}]'
  for part in "${parts[@]}"; do
    led="$part"
    local sep_before=0
    while [[ "$led" =~ $re_lead ]]; do led="${led:1}"; sep_before=1; done
    led="${led%%$re_trail*}"
    if [[ -z "$led" ]]; then at_start=1; continue; fi
    tok="${led//[\'\"]/}"
    toks+=("$tok")
    at+=( $(( at_start || sep_before )) )
    at_start=0
  done
  local i t prev
  for (( i=0; i<${#toks[@]}; i++ )); do
    t="${toks[$i]}"
    local is_start="${at[$i]}"
    local b=0
    [[ "$is_start" == "1" ]] && b=1
    if (( i>=1 )); then
      prev="${toks[$((i-1))]}"
      if [[ "$prev" == "-c" || "$prev" == "sudo" || "$prev" == "su" ]]; then b=1; fi
    fi
    local bin=0
    [[ "$t" == "coder" ]] && bin=1
    [[ "$t" == "sudo"* ]] && [[ "${t#sudo}" == "coder" ]] && bin=1
    [[ "$t" == */coder ]] && bin=1
    if (( bin && b )); then
      local j=$((i+1))
      while (( j<${#toks[@]} )) && [[ "${toks[$j]}" == -* ]]; do ((j++)); done
      if (( j<${#toks[@]} )); then
        case "${toks[$j]}" in
          templates|template|delete|create|start|stop|workspace|workspaces|server) return 0 ;;
        esac
      fi
    fi
  done
  return 1
}
if _lc_denied "$cmd"; then
  deny "workspace/template lifecycle is controller-managed — NEXUS provisions workspaces, never self-provision from a worker"
fi
exit 0
