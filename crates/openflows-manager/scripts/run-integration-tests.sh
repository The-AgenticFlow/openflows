#!/usr/bin/env bash
# Reproducible runner for the OpenFlows Manager PostgreSQL integration tests.
#
# Starts an isolated `openflows-db` PostgreSQL (if not already running), waits
# for it to accept connections, and runs the manager integration tests against
# it. The Openflows control-plane database is separate from Coder's database.
#
# Usage:
#   ./crates/openflows-manager/scripts/run-integration-tests.sh
#
# Env overrides:
#   OPENFLOWS_TEST_DATABASE_URL  - full test database URL (defaults to the
#                                  bundled openflows-db on localhost:5544)
#   OPENFLOWS_PG_PORT            - host port for the bundled openflows-db
set -euo pipefail

REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../../.." && pwd)"
cd "$REPO_ROOT"
MANAGER_CRATE="$REPO_ROOT/crates/openflows-manager"
PORT="${OPENFLOWS_PG_PORT:-5544}"
TEST_DB_URL="${OPENFLOWS_TEST_DATABASE_URL:-postgres://openflows:openflows@localhost:${PORT}/openflows_control_plane}"

if [ -z "${OPENFLOWS_TEST_DATABASE_URL:-}" ]; then
  if [ "${OPENFLOWS_PG_PASSWORD:-openflows}" != "openflows" ]; then
    echo "Set OPENFLOWS_TEST_DATABASE_URL when using a custom database password." >&2
    exit 1
  fi
  echo "==> Ensuring Openflows control-plane PostgreSQL is running on port ${PORT}"
  docker compose --profile manager up -d openflows-db
  ready=false
  for i in $(seq 1 30); do
    if docker compose --profile manager exec -T openflows-db pg_isready -U openflows -d openflows_control_plane >/dev/null 2>&1; then
      ready=true
      break
    fi
    sleep 1
  done
  if [ "$ready" != true ]; then
    echo "PostgreSQL did not become ready within 30 seconds." >&2
    exit 1
  fi
fi

echo "==> Running manager PostgreSQL integration tests (connection URL omitted)"
# The live-PostgreSQL integration tests are `#[ignore]`d so plain `cargo test`
# / CI `nextest run` (which has no database) skip them; this runner explicitly
# opts in with `--ignored` now that a database is available.
(
  cd "$MANAGER_CRATE"
  OPENFLOWS_TEST_DATABASE_URL="$TEST_DB_URL" cargo test --locked --test postgres --test http_auth --test http_org --test org_policy --test review_regressions -- --ignored --test-threads=4
  cargo test --locked --test ready --test health
)

echo "==> Done. Leave the DB running with: docker compose --profile manager up -d openflows-db"
echo "    Stop only the bundled database: docker compose --profile manager stop openflows-db"
