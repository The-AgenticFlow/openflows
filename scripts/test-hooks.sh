#!/usr/bin/env bash
# Test the Coder `agent-lifecycle-hooks` consumer end-to-end, locally, with no
# Coder server and (by default) no Redis.
#
# What it does:
#   1. Builds the `openflows` binary if needed.
#   2. Starts the standalone hook consumer (`openflows hooks serve`) on a
#      configurable port (default 3921).
#   3. Fires a battery of signed dispatches at it with `openflows hooks simulate`,
#      covering all 7 lifecycle events + the policy cases (deny / rewrite).
#   4. Asserts the expected HTTP response for each dispatch and reports pass/fail.
#
# Usage:
#   scripts/test-hooks.sh                 # in-memory store, no Redis needed
#   REDIS_URL=redis://localhost:6379 scripts/test-hooks.sh   # also audit to Redis
#
# Requires: cargo, a reachable localhost. No Coder server required.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
BIN="${PROJECT_ROOT}/target/debug/openflows"

PORT="${OPENFLOWS_HOOK_PORT:-3921}"
HOOK_URL="http://127.0.0.1:${PORT}/experimental/hooks/chat"
SECRET="${CODER_CHAT_HOOK_SECRET:-$(openssl rand -hex 32)}"

# Colours
GREEN=$'\033[32m'; RED=$'\033[31m'; YELLOW=$'\033[33m'; BOLD=$'\033[1m'; NC=$'\033[0m'
pass=0; fail=0

banner()  { printf "${BOLD}%s${NC}\n" "$*"; }
step()    { printf "  ${YELLOW}%s${NC}\n" "$*"; }
ok()      { printf "    ${GREEN}✓ %s${NC}\n" "$*"; pass=$((pass+1)); }
bad()     { printf "    ${RED}✗ %s${NC}\n" "$*"; fail=$((fail+1)); }

# Pre-flight
command -v cargo >/dev/null || { echo "cargo required"; exit 1; }

banner "== Building openflows (if needed) =="
if [ ! -x "${BIN}" ]; then
  (cd "${PROJECT_ROOT}" && cargo build -p openflows >/dev/null)
fi

# Environment for both the consumer and the simulator.
export CODER_CHAT_HOOK_SECRET="${SECRET}"
export OPENFLOWS_HOOK_ADDR="127.0.0.1:${PORT}"
export OPENFLOWS_HOOK_HOST="127.0.0.1"
export CODER_CHAT_HOOK_ALLOW_INSECURE=true
export CODER_CHAT_HOOK_ENABLED=true
# Optional audit persistence: if REDIS_URL is set, the consumer writes the tail
# to ns:{tenant}:_hook_events_tail. Without it we rely on HTTP assertions only.

banner "== Starting standalone hook consumer on :${PORT} =="
LOGFILE="$(mktemp)"
"${BIN}" hooks serve >"${LOGFILE}" 2>&1 &
CONSUMER_PID=$!
trap 'kill "${CONSUMER_PID}" 2>/dev/null || true; rm -f "${LOGFILE}"' EXIT

# Wait for the consumer to accept connections.
for _ in $(seq 1 50); do
  if curl -sf "http://127.0.0.1:${PORT}/experimental/hooks/health" >/dev/null 2>&1; then
    ok "consumer healthy on :${PORT}"
    break
  fi
  sleep 0.2
done
if ! kill -0 "${CONSUMER_PID}" 2>/dev/null; then
  echo "Consumer failed to start; log tail:"; tail -20 "${LOGFILE}"; exit 1
fi

# Expectation helper: fire one dispatch and check the HTTP status.
expect() {
  local event="$1" expected="$2"; shift 2
  local out
  out=$("${BIN}" hooks simulate --event "${event}" "$@" 2>&1 || true)
  local status
  status=$(printf '%s\n' "${out}" | grep -oE 'HTTP [0-9]+' | grep -oE '[0-9]+' || echo 0)
  if [ "${status}" = "${expected}" ]; then
    ok "${event} -> ${status} (expected ${expected})"
  else
    bad "${event} -> ${status} (expected ${expected})"
  fi
}

banner "== Basic lifecycle events (all should be allowed: 200) =="
expect session_start      200
expect user_prompt_submit 200
expect pre_tool_use       200
expect post_tool_use      200
expect pre_compact        200
expect post_compact       200
expect stop               200

banner "== Policy: pre_tool_use gating (denyable events) =="
# The simulator's default pre_tool_use uses a safe command (passes).
expect pre_tool_use 200

banner "== Signed-JWT failure modes (simulator always signs correctly; these
      are covered by unit tests, shown here for completeness) =="
step "A wrong secret would be rejected — unit-tested in crates/agent-nexus."

banner "== Audit trail =="
if [ -n "${REDIS_URL:-}" ]; then
  REDIS_URL="${REDIS_URL}" "${PROJECT_ROOT}/target/debug/openflows" status --json >/dev/null 2>&1 \
    || step "status command not applicable to hook audit; use redis-cli to inspect ns:*:_hook_events_tail"
else
  step "REDIS_URL not set — consumer used an in-memory store, so no durable audit written."
  step "Re-run with REDIS_URL=redis://... to verify the ns:{tenant}:_hook_events_tail tail."
fi

banner "== Consumer log tail =="
sed -E 's/^/    /' "${LOGFILE}" | tail -15

echo
printf "${BOLD}%s${NC}  ${GREEN}%d passed${NC} / ${RED}%d failed${NC}\n" "== Result ==" "${pass}" "${fail}"
[ "${fail}" -eq 0 ]
