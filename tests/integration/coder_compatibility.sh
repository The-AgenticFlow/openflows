#!/usr/bin/env bash
# Coder compatibility contract verification harness (WP-00).
#
# Reproducible wrapper around crates/coder-client/tests/compatibility.rs.
# Mirrors the convention of tests/integration/gated_workflow_test.sh: a thin
# shell script that selects an explicit test group and runs the ignored live
# integration tests.
#
# Test groups are selected by stable name prefixes, so a normal run NEVER
# executes the destructive cleanup group:
#
#   read-only (no mutation):
#     export CODER_URL=... CODER_SESSION_TOKEN=...
#     ./tests/integration/coder_compatibility.sh
#
#   mutating scenario + isolation (explicit opt-in, isolated org prefix):
#     export OPENFLOWS_CODER_MUTATE=1
#     export OPENFLOWS_CODER_TEST_ORG_PREFIX=ofci-$(whoami)-
#     ./tests/integration/coder_compatibility.sh --mutating
#
#   cleanup (deletes ONLY resources recorded in the ledger):
#     ./tests/integration/coder_compatibility.sh --cleanup
#
# Never targets the default organization. Never prints credentials.
set -euo pipefail
cd "$(dirname "$0")/../.."

MODE="${1:-readonly}"
case "$MODE" in
  --mutating) GROUP="mut_" ;;
  --cleanup)  GROUP="cleanup_" ;;
  readonly|-h|--help|"") GROUP="ro_" ;;
  *) echo "unknown mode '$MODE' (expected --mutating | --cleanup | readonly)" >&2; exit 2 ;;
esac

if [ -z "${CODER_URL:-}" ]; then
  echo "NOT_VERIFIED: CODER_URL is not set. No licensed Coder deployment configured." >&2
  exit 1
fi
if [ -z "${CODER_SESSION_TOKEN:-}" ] && [ -z "${CODER_SESSION_TOKEN_FILE:-}" ]; then
  echo "NOT_VERIFIED: CODER_SESSION_TOKEN (or CODER_SESSION_TOKEN_FILE) is not set." >&2
  exit 1
fi
if ! command -v coder >/dev/null 2>&1; then
  echo "NOT_VERIFIED: the 'coder' CLI is not on PATH; CLI checks cannot run." >&2
fi

# cargo flags must precede the `--` separator; the test-group filter follows it.
CARGO_FLAGS=()
if [ "$GROUP" = "mut_" ]; then
  if [ "${OPENFLOWS_CODER_MUTATE:-0}" != "1" ]; then
    echo "mutating mode requires OPENFLOWS_CODER_MUTATE=1." >&2
    exit 1
  fi
  CARGO_FLAGS=(--features coder-client/chats-api)
fi
if [ "$GROUP" = "cleanup_" ] && [ "${OPENFLOWS_CODER_MUTATE:-0}" != "1" ]; then
  echo "cleanup mode requires OPENFLOWS_CODER_MUTATE=1 (it deletes recorded test resources)." >&2
  exit 1
fi

# Note: passing arguments to cargo is verified separately from a successful
# verification run; a live deployment is still required to actually verify.
exec cargo test -p coder-client ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} --test compatibility -- --ignored "$GROUP"
