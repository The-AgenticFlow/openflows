#!/usr/bin/env bash
#
# scripts/dev-sync.sh — Keep the running nexus controller up-to-date with
# the latest source code.
#
# Usage:
#   scripts/dev-sync.sh           One-shot: rebuild + deploy if stale
#   scripts/dev-sync.sh --watch   Poll for source changes every 5s
#   scripts/dev-sync.sh --force   Rebuild + deploy unconditionally
#   scripts/dev-sync.sh --check   Exit 0 if up-to-date, exit 1 if stale
#
# What it does:
#   1. Checks if any .rs / .tf / Cargo.toml / Cargo.lock file is newer than
#      .dev-binaries/openflows. If yes (or --force):
#   2. Builds the release binary: cargo build --release -p openflows
#      (.tf templates are embedded via build.rs so they trigger a rebuild)
#   3. Copies the fresh binary to .dev-binaries/openflows (volume-mounted
#      into the nexus Coder workspace container at /opt/openflows-dev).
#   4. Finds the running openflows-nexus-* container and copies the
#      binary to /usr/local/bin/openflows inside it, then restarts
#      the controller process so the new binary takes effect immediately.
#   5. If --push-template is set, also pushes the fixed nexus template to
#      the Coder server via `coder templates push`.
#
# Requires: docker, cargo — no external file-watcher deps.
# Optional: coder CLI for --push-template (auto-detected).

set -euo pipefail

ROOT_DIR="$(cd "$(dirname "$0")/.." && pwd)"
DEV_BIN="${ROOT_DIR}/.dev-binaries/openflows"
RELEASE_BIN="${ROOT_DIR}/target/release/openflows"
WATCH_INTERVAL=5   # seconds between polls in --watch mode

# ── Helpers ─────────────────────────────────────────────────────────────

log()  { echo -e "\033[36m[dev-sync]\033[0m $*"; }
warn() { echo -e "\033[33m[dev-sync]\033[0m $*" >&2; }
err()  { echo -e "\033[31m[dev-sync]\033[0m $*" >&2; }

# Return 0 if the binary is up-to-date, 1 if stale (source is newer).
is_binary_stale() {
    # If the dev binary doesn't exist at all, it's stale.
    if [[ ! -f "$DEV_BIN" ]]; then
        log "Dev binary not found at ${DEV_BIN}"
        return 0  # stale
    fi

    # Find the newest mtime among all source files (including .tf templates
    # which are embedded into the binary via build.rs).
    local newest_src
    newest_src=$(
        find "${ROOT_DIR}/crates" "${ROOT_DIR}/binary" "${ROOT_DIR}/Cargo.toml" "${ROOT_DIR}/Cargo.lock" \
            -type f \( -name '*.rs' -o -name '*.tf' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) \
            -printf '%T@\n' 2>/dev/null | sort -rn | head -1
    )

    if [[ -z "$newest_src" ]]; then
        return 1  # can't determine, assume up-to-date
    fi

    local bin_mtime
    bin_mtime=$(stat -c %Y "$DEV_BIN" 2>/dev/null || echo 0)

    # Compare as integers (strip fractional seconds from find output)
    newest_src="${newest_src%.*}"
    bin_mtime="${bin_mtime%.*}"

    if (( newest_src > bin_mtime )); then
        log "Source files are newer than dev binary (src=${newest_src}, bin=${bin_mtime})"
        return 0  # stale
    fi

    return 1  # up-to-date
}

# Build the release binary and copy it to .dev-binaries/.
rebuild_binary() {
    log "Building release binary..."
    cd "$ROOT_DIR"

    # Check if release binary exists and is newer than all sources — if so,
    # we can skip the cargo build and just copy it.
    if [[ -f "$RELEASE_BIN" ]]; then
        local release_mtime
        release_mtime=$(stat -c %Y "$RELEASE_BIN" 2>/dev/null || echo 0)

        local newest_src
        newest_src=$(
            find "${ROOT_DIR}/crates" "${ROOT_DIR}/binary" "${ROOT_DIR}/Cargo.toml" "${ROOT_DIR}/Cargo.lock" \
                -type f \( -name '*.rs' -o -name '*.tf' -o -name 'Cargo.toml' -o -name 'Cargo.lock' \) \
                -printf '%T@\n' 2>/dev/null | sort -rn | head -1
        )

        if [[ -n "$newest_src" ]]; then
            newest_src="${newest_src%.*}"
            release_mtime="${release_mtime%.*}"
            if (( release_mtime >= newest_src )); then
                log "Release binary is already up-to-date — skipping cargo build"
            else
                cargo build --release -p openflows
            fi
        else
            cargo build --release -p openflows
        fi
    else
        cargo build --release -p openflows
    fi

    mkdir -p "${ROOT_DIR}/.dev-binaries"
    cp "$RELEASE_BIN" "$DEV_BIN"
    chmod +x "$DEV_BIN"
    log "Copied binary to ${DEV_BIN}"
}

# Find the running nexus container and deploy the new binary into it.
deploy_to_container() {
    local nexus_container
    nexus_container=$(docker ps --filter "name=openflows-nexus-" --format '{{.Names}}' | head -1)

    if [[ -z "$nexus_container" ]]; then
        warn "No running openflows-nexus-* container found"
        warn "Binary updated in .dev-binaries/ — it will be picked up on next workspace start"
        return 0
    fi

    log "Found running container: ${nexus_container}"

    # Copy the binary into the container (the bind mount is read-only,
    # so we docker cp to /usr/local/bin directly).
    if docker cp "$DEV_BIN" "${nexus_container}:/usr/local/bin/openflows"; then
        docker exec "$nexus_container" chmod +x /usr/local/bin/openflows 2>/dev/null || true
        log "Binary deployed to container"
    else
        err "Failed to copy binary into container ${nexus_container}"
        return 1
    fi

    # Restart the controller process inside the container.
    # The startup script launches it with `nohup openflows run`, so we kill
    # the existing process and relaunch.
    log "Restarting controller in ${nexus_container}..."
    docker exec "$nexus_container" bash -c '
        # Kill the old controller (graceful then forceful)
        pkill -TERM -x openflows 2>/dev/null || true
        sleep 2
        pkill -KILL -x openflows 2>/dev/null || true
        sleep 1
        # Relaunch with the same env it had originally
        cd /home/coder/workspace
        nohup openflows run > /tmp/openflows-controller.log 2>&1 &
        echo "Controller restarted, PID: $!"
    ' 2>&1 | while read -r line; do log "$line"; done

    log "Controller restarted — monitoring logs for 5s..."
    sleep 1
    docker exec "$nexus_container" tail -5 /tmp/openflows-controller.log 2>/dev/null | while read -r line; do log "$line"; done
}

# Check if any .tf template files changed since the last sync.
tf_templates_changed() {
    if [[ ! -f "$DEV_BIN" ]]; then
        return 0  # everything changed
    fi
    local bin_mtime
    bin_mtime=$(stat -c %Y "$DEV_BIN" 2>/dev/null || echo 0)
    bin_mtime="${bin_mtime%.*}"

    local newest_tf
    newest_tf=$(
        find "${ROOT_DIR}/crates/coder-client/templates" \
            -type f -name '*.tf' \
            -printf '%T@\n' 2>/dev/null | sort -rn | head -1
    )
    if [[ -z "$newest_tf" ]]; then
        return 1  # no tf files found
    fi
    newest_tf="${newest_tf%.*}"
    (( newest_tf > bin_mtime ))
}

# Push the nexus template to the Coder server so new workspaces use the
# latest startup script.  Requires the coder CLI and CODER_SESSION_TOKEN.
push_nexus_template() {
    if ! command -v coder >/dev/null 2>&1; then
        warn "coder CLI not found — skipping template push"
        warn "Install coder CLI or run 'coder templates push openflows-nexus' manually"
        return 0
    fi

    if [[ -z "${CODER_URL:-}" ]]; then
        warn "CODER_URL not set — skipping template push"
        return 0
    fi

    if [[ -z "${CODER_SESSION_TOKEN:-}" ]]; then
        warn "CODER_SESSION_TOKEN not set — skipping template push"
        warn "Set CODER_SESSION_TOKEN or run 'coder login' first"
        return 0
    fi

    local template_dir="${ROOT_DIR}/crates/coder-client/templates/openflows-nexus"
    local host_path="${ROOT_DIR}/.dev-binaries"

    log "Pushing nexus template to Coder (this may take a minute for provider downloads)..."
    if coder templates push --yes openflows-nexus \
        -d "$template_dir" \
        --variable "dev_binary_host_path=${host_path}" 2>&1 | while read -r line; do log "$line"; done; then
        log "Template pushed successfully"
    else
        warn "Template push failed — workspaces started from the old template may have stale startup scripts"
    fi
}

# ── Main ────────────────────────────────────────────────────────────────

sync_once() {
    local do_rebuild=false
    if is_binary_stale || [[ "${FORCE:-}" == "1" ]]; then
        do_rebuild=true
    fi

    local do_push_template=false
    if [[ "${PUSH_TEMPLATE:-}" == "1" ]] && tf_templates_changed; then
        do_push_template=true
    fi

    if $do_rebuild; then
        log "Binary is stale — rebuilding"
        rebuild_binary
        deploy_to_container
    else
        log "Binary is up-to-date — nothing to do"
    fi

    if $do_push_template; then
        push_nexus_template
    fi
}

# Parse args
MODE="once"
while [[ $# -gt 0 ]]; do
    case "$1" in
        --watch)          MODE="watch";  shift ;;
        --force)          FORCE=1;       shift ;;
        --check)          MODE="check";  shift ;;
        --push-template)  PUSH_TEMPLATE=1; shift ;;
        *)                err "Unknown option: $1"; exit 1 ;;
    esac
done

case "$MODE" in
    check)
        if is_binary_stale; then
            log "Binary is STALE — rebuild needed"
            exit 1
        else
            log "Binary is up-to-date"
            exit 0
        fi
        ;;
    once)
        sync_once
        ;;
    watch)
        log "Watching for source changes (poll every ${WATCH_INTERVAL}s, Ctrl+C to stop)..."
        log "Initial sync..."
        sync_once
        log "Watching..."
        while true; do
            sleep "$WATCH_INTERVAL"
            if is_binary_stale; then
                log "Source change detected!"
                sync_once
            fi
        done
        ;;
esac