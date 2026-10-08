#!/bin/bash
# Run only inside a Sysbox worker. Coder supplies the agent command as argv.
set -euo pipefail

fail() { echo "[worker-engine] $*" >&2; exit 1; }
[[ $# -gt 0 ]] || fail 'Missing Coder agent command'
[[ $(id -u) == 0 ]] || fail 'Entrypoint must start as container root'
# Fail closed on ordinary rootful containers. This is a sanity check; the
# provisioning template, not guest code, enforces the selected runtime.
awk '$1 == 0 && $2 != 0 { found=1 } END { exit !found }' /proc/self/uid_map \
    || fail 'A remapped root user is required; configure sysbox-runc on the host'
mountpoint -q /var/run/docker.sock && fail 'Refusing a mounted Docker socket'
# A crashed container can leave runtime files in its writable layer. The
# template never mounts /run; remove stale files before restarting our daemon.
rm -f /var/run/docker.sock /var/run/docker.pid
unset DOCKER_CONTEXT DOCKER_TLS_VERIFY DOCKER_CERT_PATH
export DOCKER_HOST=unix:///var/run/docker.sock
export TESTCONTAINERS_HOST_OVERRIDE=localhost

engine_pid=''
agent_pid=''
cleanup() {
    trap - EXIT TERM INT
    for pid in "$agent_pid" "$engine_pid"; do
        if [[ -n "$pid" ]]; then kill -TERM -- "-$pid" 2>/dev/null || true; fi
    done
    # Bound shutdown even when an agent or daemon ignores TERM.
    for ((i=0; i<10; i++)); do
        local alive=false
        for pid in "$agent_pid" "$engine_pid"; do
            if [[ -n "$pid" ]] && kill -0 -- "-$pid" 2>/dev/null; then alive=true; fi
        done
        "$alive" || break
        sleep 1
    done
    for pid in "$agent_pid" "$engine_pid"; do
        if [[ -n "$pid" ]]; then kill -KILL -- "-$pid" 2>/dev/null || true; fi
    done
    wait 2>/dev/null || true
}
trap cleanup EXIT
trap 'exit 143' TERM
trap 'exit 130' INT

# No TCP listener, host socket mount, or world-writable socket. The docker
# group grants coder control of this workspace-local daemon only.
setsid dockerd --host="$DOCKER_HOST" --group=docker --data-root=/var/lib/docker \
    --storage-driver=overlay2 --feature=containerd-snapshotter=false &
engine_pid=$!
ready=false
for ((i=0; i<60; i++)); do
    kill -0 "$engine_pid" 2>/dev/null || fail 'Docker daemon exited during startup'
    if timeout 2 runuser -u coder -- docker info >/dev/null 2>&1; then
        ready=true
        break
    fi
    sleep 1
done
"$ready" || fail 'Docker did not become ready for coder'
echo '[worker-engine] Workspace Docker daemon ready' >&2
setsid runuser -u coder -- "$@" &
agent_pid=$!
# Exit when either service exits; never leave an apparently working agent
# after its engine has crashed.
set +e
wait -n "$engine_pid" "$agent_pid"
status=$?
set -e
if ! kill -0 "$engine_pid" 2>/dev/null; then
    fail 'Docker daemon stopped'
fi
exit "$status"
