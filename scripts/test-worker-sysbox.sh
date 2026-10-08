#!/bin/bash
# Opt-in integration test on a Linux Docker host with Sysbox installed.
set -euo pipefail
image=${1:?Usage: bash scripts/test-worker-sysbox.sh IMAGE}
root=$(cd "$(dirname "$0")/.." && pwd)
docker info --format '{{json .Runtimes}}' | python3 -c \
    'import json,sys; assert "sysbox-runc" in json.load(sys.stdin), "Install Sysbox on the Docker host first"'
prefix="openflows-sysbox-test-$(date +%s)-$$"
cleanup() {
    docker rm -f "$prefix-a" "$prefix-b" >/dev/null 2>&1 || true
    docker volume rm "$prefix-a" "$prefix-b" >/dev/null 2>&1 || true
}
trap cleanup EXIT
for suffix in a b; do
    docker volume create "$prefix-$suffix" >/dev/null
    docker run -d --name "$prefix-$suffix" --runtime=sysbox-runc \
        --platform linux/amd64 -v "$prefix-$suffix:/var/lib/docker" \
        "$image" sleep infinity >/dev/null
    ready=false
    for ((i=0; i<90; i++)); do
        if docker exec -u coder "$prefix-$suffix" docker info >/dev/null 2>&1; then
            ready=true; break
        fi
        sleep 1
    done
    if ! "$ready"; then docker logs "$prefix-$suffix"; exit 1; fi
    test "$(docker exec "$prefix-$suffix" stat -c %a /var/run/docker.sock)" = 660
done
engine_a=$(docker exec -u coder "$prefix-a" docker info --format '{{.ID}}')
engine_b=$(docker exec -u coder "$prefix-b" docker info --format '{{.ID}}')
host_engine=$(docker info --format '{{.ID}}')
test "$engine_a" != "$engine_b"
test "$engine_a" != "$host_engine"
test "$engine_b" != "$host_engine"
docker exec -u coder "$prefix-a" docker run -d --name private-marker alpine:3.21 sleep 300 >/dev/null
if docker exec -u coder "$prefix-b" docker inspect private-marker >/dev/null 2>&1; then
    echo 'Sibling workspace can see private container' >&2; exit 1
fi
# A container restart must restore the engine and retain its private storage.
docker restart "$prefix-a" >/dev/null
ready=false
for ((i=0; i<90; i++)); do
    if docker exec -u coder "$prefix-a" docker info >/dev/null 2>&1; then
        ready=true; break
    fi
    sleep 1
done
if ! "$ready"; then docker logs "$prefix-a"; exit 1; fi
docker exec -u coder "$prefix-a" docker inspect private-marker >/dev/null
test "$(docker exec -u coder "$prefix-a" docker info --format '{{.ID}}')" = "$engine_a"
for suffix in a b; do
    docker exec "$prefix-$suffix" bash -c \
        'apt-get update -qq && apt-get install -y -qq python3-venv'
    docker exec -u coder "$prefix-$suffix" bash -c \
        'python3 -m venv /tmp/container-test && /tmp/container-test/bin/pip install testcontainers==4.10.0 psycopg2-binary==2.9.10'
    docker cp "$root/tests/worker/testcontainers_smoke.py" "$prefix-$suffix:/tmp/smoke.py"
    docker exec -u coder "$prefix-$suffix" /tmp/container-test/bin/python /tmp/smoke.py
done
echo 'Both worker engines passed Testcontainers and daemon separation checks'
