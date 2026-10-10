#!/usr/bin/env bash
# Run only against a new, disposable Docker stack; no production credentials.
set -euo pipefail
cd "$(dirname "$0")/../.."
project="openflows-ci-$(date +%s)-$$"
suite=${OPENFLOWS_E2E_SUITE:-worker}
case "$suite" in
    worker|verification) ;;
    *) echo "Unknown E2E suite: $suite" >&2; exit 2 ;;
esac
artifacts="$PWD/target/ci-artifacts/coder"
if [ "$suite" = verification ]; then
    artifacts="$PWD/target/ci-artifacts/delegated-verification"
fi
artifacts="$artifacts/$project"
mkdir -p "$artifacts"
scratch=$(mktemp -d)
compose=(docker compose --env-file /dev/null -p "$project" -f tests/e2e/compose.yml)
cleanup() {
    result=$?
    trap - EXIT
    "${compose[@]}" logs --no-color >"$artifacts/stack.log" 2>&1 || true
    while IFS= read -r id; do
        [ -n "$id" ] || continue
        docker logs "$id" >"$artifacts/worker-$id.log" 2>&1 || true
        docker exec "$id" cat /tmp/verify.log >"$artifacts/verify-$id.log" 2>&1 || true
        docker rm -f "$id" >/dev/null 2>&1 || result=1
    done < <(docker ps -aq --filter "label=openflows.ci.project=$project")
    "${compose[@]}" down --volumes --remove-orphans >/dev/null 2>&1 || result=1
    docker image rm "$project-worker:ci" >/dev/null 2>&1 || true
    rm -rf "$scratch"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM

cargo build --locked -p openflows-harness --bin openflows-harness 2>&1 | tee "$artifacts/build.log"
cargo build --locked -p openflows --example ci_a2a_relay 2>&1 | tee -a "$artifacts/build.log"
cp target/debug/openflows-harness "$scratch/"
cp target/debug/examples/ci_a2a_relay "$scratch/"
cp tests/e2e/worker.Dockerfile "$scratch/Dockerfile"
docker build -t "$project-worker:ci" "$scratch" 2>&1 | tee "$artifacts/image.log"
"${compose[@]}" up -d --wait --wait-timeout 120
if [ "$suite" = verification ]; then
    relay=$(docker run -d --network "${project}_default" --network-alias a2a-relay \
        --label "openflows.ci.project=$project" \
        -e REDIS_URL=redis://redis:6379 -e OPENFLOWS_TENANT=ci-verify \
        -e A2A_RELAY_ADDR=0.0.0.0:3000 \
        -e A2A_PAIR_TOKEN=disposable-ci-pair-token-not-a-production-secret \
        --health-cmd 'curl -fsS http://127.0.0.1:3000/health' \
        --health-interval 1s --health-timeout 2s --health-retries 30 \
        "$project-worker:ci" ci-a2a-relay)
    for attempt in $(seq 1 40); do
        health=$(docker inspect "$relay" --format '{{.State.Health.Status}}')
        [ "$health" != unhealthy ] || exit 1
        [ "$health" != healthy ] || break
        sleep 1
    done
    [ "$health" = healthy ] || exit 1
fi
coder_id=$("${compose[@]}" ps -q coder)
# Use the exact CLI shipped in the pinned server image.
docker cp "$coder_id:/opt/coder" "$scratch/coder"
chmod +x "$scratch/coder"
export PATH="$scratch:$PATH"
export CODER_CONFIG_DIR="$scratch/config"
export OPENFLOWS_E2E_CODER_URL="http://$("${compose[@]}" port coder 7080)"
export TEST_REDIS_URL="redis://$("${compose[@]}" port redis 6379)"
export TF_VAR_dev_binary_host_path="$project"
tar -czf "$scratch/template.tar.gz" -C tests/e2e/template .
export OPENFLOWS_E2E_TEMPLATE_ARCHIVE="$scratch/template.tar.gz"
export OPENFLOWS_E2E_ARTIFACTS="$artifacts"
if [ "$suite" = verification ]; then
    package=openflows
    test=delegated_verify_e2e
else
    package=coder-client
    test=container_e2e
fi
timeout 1200 cargo test --locked -p "$package" --test "$test" -- \
    --ignored --nocapture --test-threads=1 2>&1 | tee "$artifacts/tests.log"
