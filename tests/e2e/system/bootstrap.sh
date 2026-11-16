#!/usr/bin/env bash
# Runs production bootstrap or the connected issue-to-merge journey in an isolated stack.
# Saves diagnostics before deleting run-owned resources.

set -euo pipefail
cd "$(dirname "$0")/../../.."
scenario="${1:-bootstrap}"
case "$scenario" in
    bootstrap) category=production-bootstrap; driver=production_bootstrap.py; model=bootstrap-model.json ;;
    system) category=connected-system; driver=connected_system.py; model=system-model.json ;;
    # Upgrade, restart and rollback of a running stack (#528); Redis persists as in production.
    upgrade) category=upgrade-rollback; driver=upgrade_rollback.py; model=bootstrap-model.json ;;
    # One candidate release installed on a clean stack in one mode (#527): source, compose or binary.
    install)
        export OPENFLOWS_E2E_MODE="${2:-}"
        case "$OPENFLOWS_E2E_MODE" in
            source|compose|binary) ;;
            *) printf 'Usage: %s install source|compose|binary\n' "$0" >&2; exit 2 ;;
        esac
        category="clean-install-$OPENFLOWS_E2E_MODE"; driver=clean_install.py; model=bootstrap-model.json ;;
    workspace-runtime)
        category=workspace-runtime; driver=workspace_runtime_e2e.py; model=bootstrap-model.json ;;
    *) printf 'Unknown E2E scenario: %s\n' "$scenario" >&2; exit 2 ;;
esac
project="openflows-${scenario}-$(date +%s)-$$"
artifacts="$PWD/target/ci-artifacts/$category/$project"
mkdir -p "$artifacts"
scratch=$(mktemp -d)
export OPENFLOWS_SYSTEM_FIXTURE_IMAGE="$project-fixtures:ci"
export OPENFLOWS_E2E_MODEL_SCENARIO="/opt/fixtures/$model"
compose=(docker compose --env-file /dev/null -p "$project" -f "$PWD/tests/e2e/compose.yml" -f "$PWD/tests/e2e/system/bootstrap.compose.yml")
[ "$scenario" != upgrade ] || compose+=(-f "$PWD/tests/e2e/system/upgrade.compose.yml")
[ "$scenario" != workspace-runtime ] || compose+=(-f "$PWD/tests/e2e/system/manager.compose.yml")
remove_resource() {
    local kind="$1" name="$2" filter remaining
    if [ "$kind" = image ]; then
        filter="reference=$name"
    else
        filter="name=^${name}$"
    fi
    # Removal may legitimately find a resource already removed by Compose.
    docker "$kind" rm "$name" >>"$artifacts/cleanup.log" 2>&1 || true
    if ! remaining=$(docker "$kind" ls --quiet --filter "$filter" 2>>"$artifacts/cleanup.log"); then
        return 1
    fi
    if [ -n "$remaining" ]; then
        printf 'Cleanup left %s %s behind\n' "$kind" "$name" >>"$artifacts/cleanup.log"
        return 1
    fi
}
cleanup() {
    result=$?
    trap - EXIT
    : >"$artifacts/cleanup.log"
    "${compose[@]}" logs --no-color >"$artifacts/stack.log" 2>&1 || true
    # Only this run's private network can contain these resources.
    while IFS= read -r id; do
        [ -n "$id" ] || continue
        docker logs "$id" >"$artifacts/container-$id.log" 2>&1 || true
        docker cp "$id:/home/coder/.local/state/openflows/verify.log" "$artifacts/verify-$id.log" 2>/dev/null || true
        docker exec "$id" cat /tmp/openflows-controller.log >"$artifacts/controller-$id.log" 2>&1 || true
        docker cp "$id:/tmp/oauth/oauth-requests.jsonl" "$artifacts/oauth-requests.jsonl" 2>/dev/null || true
        docker cp "$id:/tmp/model/model-requests.jsonl" "$artifacts/model-requests.jsonl" 2>/dev/null || true
        docker cp "$id:/tmp/github/github-requests.jsonl" "$artifacts/github-requests.jsonl" 2>/dev/null || true
        docker cp "$id:/tmp/github/ci-artifacts" "$artifacts/ci" 2>/dev/null || true
        docker cp "$id:/tmp/github/repo.git" "$artifacts/repo.git" 2>/dev/null || true
        docker inspect "$id" --format '{{range .Mounts}}{{if eq .Type "volume"}}{{println .Name}}{{end}}{{end}}' >>"$scratch/volumes" || true
        docker rm -f "$id" >>"$artifacts/cleanup.log" 2>&1 || result=1
    done < <(docker ps -aq --filter "network=${project}_default")
    "${compose[@]}" down --volumes --remove-orphans >>"$artifacts/cleanup.log" 2>&1 || result=1
    if [ -f "$scratch/volumes" ]; then
        while IFS= read -r volume; do
            [ -n "$volume" ] || continue
            remove_resource volume "$volume" || result=1
        done < <(sort -u "$scratch/volumes")
    fi
    if [ "$scenario" != workspace-runtime ]; then
        remove_resource image "$project-workspace:ci" || result=1
        remove_resource image "$project-runtime:ci" || result=1
    fi
    [ "$scenario" != upgrade ] || remove_resource image "$project-workspace:ci-upgrade" || result=1
    remove_resource image "$OPENFLOWS_SYSTEM_FIXTURE_IMAGE" || result=1
    rm -rf "$scratch"
    exit "$result"
}
trap cleanup EXIT
trap 'exit 130' INT
trap 'exit 143' TERM
if [ "$scenario" = workspace-runtime ]; then
    cargo build --locked -p openflows --bin openflows -p openflows-manager --bin openflows-manager 2>&1 | tee "$artifacts/build.log"
    mkdir -p "$scratch/bin" "$scratch/dev-bin" "$scratch/home"
    cp target/debug/openflows-manager "$scratch/bin/"
    cp target/debug/openflows "$scratch/dev-bin/"
    mkdir -p "$scratch/dev-bin/runtime"
    cp -r runtime/lib runtime/bin runtime/VERSION "$scratch/dev-bin/runtime/"
else
    cargo build --locked -p openflows --bin openflows --bin openflows-doctor -p openflows-harness --bin openflows-harness 2>&1 | tee "$artifacts/build.log"
    mkdir -p "$scratch/bin" "$scratch/home"
    cp target/debug/openflows target/debug/openflows-doctor target/debug/openflows-harness "$scratch/bin/"
fi
docker build -t "$OPENFLOWS_SYSTEM_FIXTURE_IMAGE" tests/e2e/system 2>&1 | tee "$artifacts/fixtures-image.log"
if [ "$scenario" != workspace-runtime ]; then
    # The workspace image is the OpenFlows runtime image built from this checkout with the
    # candidate programs, so workspaces run them without a development mount.
    runtime/build-image.sh "$project-runtime:ci" "$scratch/bin" 2>&1 | tee "$artifacts/runtime-image.log"
    docker build --build-arg RUNTIME_IMAGE="$project-runtime:ci" -t "$project-workspace:ci" \
        -f tests/e2e/system/workspace.Dockerfile tests/e2e/system 2>&1 | tee "$artifacts/workspace-image.log"
fi
export TF_VAR_docker_network="${project}_default"
export TF_VAR_github_api_base=http://github-fixture:8080
export TF_VAR_github_git_base="git://github-fixture:9418"
if [ "$scenario" = workspace-runtime ]; then
    export TF_VAR_dev_binary_host_path="$scratch/dev-bin"
    export TF_VAR_connected_local_manager_url=http://openflows-manager:3002
else
    export TF_VAR_dev_binary_host_path="$scratch/bin"
    export TF_VAR_connected_local_manager_url=http://github-fixture:8080
fi
printf '%s' ci-broker-key >"$scratch/broker-key"
chmod 600 "$scratch/broker-key"
export TF_VAR_connected_local=true
export TF_VAR_connected_local_broker_secret_host_path="$scratch/broker-key"
export TF_VAR_connected_local_helper_host_path="$PWD/scripts/connected-local-github.py"
"${compose[@]}" up -d --wait --wait-timeout 120
coder_id=$("${compose[@]}" ps -q coder)
docker cp "$coder_id:/opt/coder" "$scratch/bin/coder"
chmod +x "$scratch/bin/coder"
export PATH="$scratch/bin:$PATH"
export OPENFLOWS_HOME="$scratch/home/.openflows"
unset OPENFLOWS_REGISTRY_PATH OPENFLOWS_REGISTRY_JSON
export RUST_LOG=info
export CODER_CONFIG_DIR="$scratch/home/coder"
export CODER_URL="http://$("${compose[@]}" port coder 7080)"
export CODER_ADMIN_EMAIL=ci@example.test
export CODER_ADMIN_USERNAME=ci
export CODER_ADMIN_PASSWORD=Disposable-CI-password-729!
export REDIS_URL="redis://$("${compose[@]}" port redis 6379)"
export CODER_CHAT_HOOK_SECRET=disposable-ci-hook-secret-at-least-32-bytes
export CODER_CHAT_HOOK_URL=http://openflows-nexus:3001/experimental/hooks/chat
export GITHUB_GIT_BASE=git://github-fixture:9418
export TF_VAR_workspace_image="$project-workspace:ci"
if [ "$scenario" = workspace-runtime ]; then
    export OPENFLOWS_E2E_MANAGER_DB_URL="postgres://openflows:openflows@$("${compose[@]}" port openflows-db 5432)/openflows_control_plane"
fi
export OPENFLOWS_E2E_OAUTH_URL="http://$("${compose[@]}" port oauth 8080)"
export OPENFLOWS_E2E_ARTIFACTS="$artifacts"
export OPENFLOWS_E2E_GITHUB_URL="http://$("${compose[@]}" port github-fixture 8080)"
export OPENFLOWS_E2E_GITHUB_CONTAINER="$("${compose[@]}" ps -q github-fixture)"
# Keep the prefixed Nexus workspace name within Coder's 32-character limit.
export OPENFLOWS_E2E_TENANT="ci-$(printf '%s' "$project" | sha256sum | cut -c1-12)"
# The upgrade driver restarts services and runs setup from a copy of this checkout.
export OPENFLOWS_E2E_PROJECT="$project" OPENFLOWS_E2E_CHECKOUT="$PWD" OPENFLOWS_E2E_SCRATCH="$scratch"
driver="$PWD/tests/e2e/system/$driver"
# Avoid loading the developer's .env or persisted bootstrap credentials.
cd "$scratch"
timeout 1500 python3 "$driver" </dev/null 2>&1 | tee "$artifacts/test.log"
