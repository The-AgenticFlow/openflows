#!/usr/bin/env bash
# Fixture contracts only. The full production controller journey is the next stage.
set -euo pipefail
cd "$(dirname "$0")/../../.."
project="openflows-fixtures-$(date +%s)-$$"
artifacts="$PWD/target/ci-artifacts/system-fixtures/$project"
mkdir -p "$artifacts"
cleanup() {
    result=$?
    trap - EXIT
    docker logs "$project" >"$artifacts/container.log" 2>&1 || true
    docker rm -f "$project" >/dev/null 2>&1 || result=1
    docker image rm "$project:ci" >/dev/null 2>&1 || true
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

OPENFLOWS_FIXTURE_ARTIFACTS="$artifacts/production-client" \
    timeout 600 cargo test --locked -p github --test system_fixture_contract -- --nocapture \
    2>&1 | tee "$artifacts/production-client.log"
timeout 300 docker build -t "$project:ci" tests/e2e/system \
    2>&1 | tee "$artifacts/build.log"
# No network egress or production mounts. Loopback HTTP/Git remains available.
timeout 120 docker run --name "$project" --label "openflows.ci.project=$project" \
    --network none --user "$(id -u):$(id -g)" \
    -e OPENFLOWS_FIXTURE_ARTIFACTS=/artifacts \
    --mount "type=bind,src=$artifacts,dst=/artifacts" \
    "$project:ci" 2>&1 | tee "$artifacts/tests.log"
