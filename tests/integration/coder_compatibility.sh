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
#   mutating scenario (creates the two isolation orgs + workspace fixtures,
#   explicit opt-in, isolated org prefix):
#     export OPENFLOWS_CODER_MUTATE=1
#     export OPENFLOWS_CODER_TEST_ORG_PREFIX=ofci-$(whoami)-
#     ./tests/integration/coder_compatibility.sh --mutating
#
#   isolation check (MUST run after --mutating populated the fixtures; the
#   wrapper sources the fixture manifest written by the scenario):
#     ./tests/integration/coder_compatibility.sh --isolation
#
#   cleanup (deletes ONLY resources recorded in the ledger):
#     ./tests/integration/coder_compatibility.sh --cleanup
#
# Never targets the default organization. Never prints credentials.
set -euo pipefail
cd "$(dirname "$0")/../.."

MODE="${1:-readonly}"
case "$MODE" in
  --mutating)  GROUP="mut_" ;;
  --isolation) GROUP="iso_" ;;
  --cleanup)   GROUP="cleanup_" ;;
  readonly|-h|--help|"") GROUP="ro_" ;;
  *) echo "unknown mode '$MODE' (expected --mutating | --isolation | --cleanup | readonly)" >&2; exit 2 ;;
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
if [ "$GROUP" = "iso_" ]; then
  # Isolation needs the fixtures created by --mutating and the machine-identity
  # tokens minted by the scenario. Source the manifest the scenario wrote.
  MANIFEST="${TMPDIR:-/tmp}/ofci-isolation-manifest.env"
  if [ -f "$MANIFEST" ]; then
    # shellcheck disable=SC1090
    . "$MANIFEST"
  else
    echo "NOT_VERIFIED: isolation fixture manifest not found at $MANIFEST." >&2
    echo "Run the --mutating scenario first so it can create the two orgs," >&2
    echo "workspace fixtures, and identity tokens." >&2
    exit 1
  fi
  CARGO_FLAGS=(--features coder-client/chats-api)
fi
if { [ "$GROUP" = "cleanup_" ] || [ "$GROUP" = "iso_" ]; } && [ "${OPENFLOWS_CODER_MUTATE:-0}" != "1" ]; then
  echo "$GROUP mode requires OPENFLOWS_CODER_MUTATE=1." >&2
  exit 1
fi

# Note: passing arguments to cargo is verified separately from a successful
# verification run; a live deployment is still required to actually verify.
exec cargo test -p coder-client ${CARGO_FLAGS[@]+"${CARGO_FLAGS[@]}"} --test compatibility -- --ignored "$GROUP"
