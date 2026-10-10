#!/usr/bin/env bash
# Real Redis lifecycle integration; never target an existing production Redis.
set -euo pipefail
cd "$(dirname "$0")/../.."
container="openflows-lifecycle-test-$$"
artifacts="$PWD/target/ci-artifacts/redis"
mkdir -p "$artifacts"
cleanup() {
    docker logs "$container" >"$artifacts/redis.log" 2>&1 || true
    docker rm -f "$container" >/dev/null 2>&1 || true
}
trap cleanup EXIT
# The test injects OOM errors, so always provision an isolated disposable server.
docker run --rm -d --name "$container" -p 127.0.0.1::6379 redis:7-alpine@sha256:858f009f9709ce576febc734aa78b8f6d624b82571f9ddb6bda4377c833b3499 >/dev/null
endpoint=$(docker port "$container" 6379/tcp)
export TEST_REDIS_URL="redis://$endpoint"
ready=false
for _ in $(seq 1 30); do
    if docker exec "$container" redis-cli ping | grep -qx PONG; then
        ready=true
        break
    fi
    sleep 1
done
if [ "$ready" != true ]; then
    echo "Disposable Redis did not become ready." >&2
    exit 1
fi
# One test changes global Redis maxmemory. Tenant prefixes do not isolate that
# setting: serialize the tests so fault injection cannot affect another test.
cargo test --locked -p openflows-harness --test lifecycle_redis -- \
    --ignored --test-threads=1 --nocapture 2>&1 | tee "$artifacts/tests.log"
