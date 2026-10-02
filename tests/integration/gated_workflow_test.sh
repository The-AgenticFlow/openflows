#!/usr/bin/env bash
# Real Redis lifecycle integration; never target an existing production Redis.
set -euo pipefail
cd "$(dirname "$0")/../.."
container="openflows-lifecycle-test-$$"
cleanup() { docker rm -f "$container" >/dev/null 2>&1 || true; }
trap cleanup EXIT
# The test injects OOM errors, so always provision an isolated disposable server.
docker run --rm -d --name "$container" -p 127.0.0.1::6379 redis:7-alpine >/dev/null
endpoint=$(docker port "$container" 6379/tcp)
export TEST_REDIS_URL="redis://$endpoint"
cargo test -p openflows-harness --test lifecycle_redis -- --ignored
