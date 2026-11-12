#!/usr/bin/env bash
# Sourced by setup.sh: starts the Docker stack and connects GitHub through the local Manager.

# ── stage 4: Docker stack ───────────────────────────────────────────────────
# compose_files — the -f arguments for this install, one per line; none for plain `docker compose`.
# connected-local adds the local Manager (from its image unless compiling); the images
# file copies the programs into .dev-binaries before Coder starts.
compose_files() {
    local extra=()
    if [ "$(env_get OPENFLOWS_GITHUB_AUTH_MODE)" = connected-local ]; then
        extra+=(-f docker-compose.connected-local.yml)
        [ "$MODE" = source ] || extra+=(-f docker-compose.manager-image.yml)
    fi
    [ "$MODE" = compose ] && extra+=(-f docker-compose.images.yml)
    if [ "${#extra[@]}" -gt 0 ]; then
        compose_base_files
        printf '%s\n' "${extra[@]}"
    fi
}
# shared_app_leftover — the local Manager is still here from before switching to your own App.
# A database without a Manager was started by hand (see .env.example), so it doesn't count.
shared_app_leftover() {
    [ "$(env_get OPENFLOWS_GITHUB_AUTH_MODE)" != connected-local ] || return 1
    (cd "$ROOT" && docker compose ps -a --format '{{.Service}}' 2>/dev/null) | grep -x openflows-local-manager >/dev/null
}
stage_docker() {
    step "Starting Redis + Coder"
    mkdir -p "$BIN_DIR" 2>/dev/null || true   # by us: when Docker creates it for its mount, root owns it
    [ -w "$BIN_DIR" ] || die "Cannot write to .dev-binaries." "Make it yours:  sudo chown -R \"\$(id -u):\$(id -g)\" ${BIN_DIR}"
    local out i line args=() leftover=""
    while IFS= read -r line; do args+=("$line"); done < <(compose_files)
    args+=(up -d)
    if shared_app_leftover; then leftover=yes; args+=(--remove-orphans); fi
    [ "$(env_get OPENFLOWS_GITHUB_AUTH_MODE)" = connected-local ] && [ "$MODE" = source ] && args+=(--build)
    # Compose prefers shell variables over .env; pass the image from .env so a leftover
    # export (e.g. from an earlier upgrade) cannot copy a different version.
    local image_env=()
    [ "$MODE" = compose ] && image_env=(
        "OPENFLOWS_VERSION=$(env_get OPENFLOWS_VERSION)"
        "OPENFLOWS_IMAGE_DIGEST=$(env_get OPENFLOWS_IMAGE_DIGEST)"
        "OPENFLOWS_HOST_UID=$(env_get OPENFLOWS_HOST_UID)"
        "OPENFLOWS_HOST_GID=$(env_get OPENFLOWS_HOST_GID)")
    [ "$MODE" = source ] || image_env+=("OPENFLOWS_MANAGER_IMAGE=$(env_get OPENFLOWS_MANAGER_IMAGE)")
    if ! out="$(cd "$ROOT" && env ${image_env[@]+"${image_env[@]}"} docker compose "${args[@]}" 2>&1)"; then
        printf '%s\n' "$out" | tail -8 | sed 's/^/    /' >&2
        die "docker compose up failed (see above)."
    fi
    if [ -n "$leftover" ]; then
        # Its profile keeps the database out of --remove-orphans. Its data stays for switching back.
        if (cd "$ROOT" && docker compose --profile manager rm -fs openflows-db) >"$LOG_FILE" 2>&1; then
            ok "Removed the local Manager, which your own App doesn't use"
        else
            warn "Could not remove openflows-db. Run: docker compose --profile manager rm -fs openflows-db"
        fi
    fi
    ( for ((i = 0; i < 90; i++)); do curl -fs -m 3 "$(coder_url)/api/v2/buildinfo" >/dev/null 2>&1 && exit 0; sleep 2; done; exit 1 ) &
    spin $! "Waiting for Coder to become healthy" \
        || die "Coder did not become healthy in 3 minutes." "Check logs: docker compose logs coder"
    if [ "$MODE" = compose ]; then
        [ "$(cat "${BIN_DIR}/IMAGE" 2>/dev/null)" = "$(image_ref)" ] \
            || die "The OpenFlows programs were not copied from $(image_ref)." \
                "Check logs: docker compose -f docker-compose.yml -f docker-compose.images.yml logs openflows-binaries"
        "$OPENFLOWS_BIN" --version >/dev/null 2>&1 \
            || die "The OpenFlows programs do not run on this machine." \
                "Use the source install instead:  ./scripts/setup.sh ${REPO} --source"
        ok "programs installed from the ${B}$(env_get OPENFLOWS_VERSION)${N} image"
    fi
    ok "Coder is up at ${B}$(coder_url)${N}"
}

# stage_connected_local_authorization — the team App is configured by the
# OpenFlows release. Contributors install it, approve their own GitHub account,
# and choose the requested repository in the local browser console.
manager_repo_list() { # the repositories available to the signed-in GitHub user and the App
    local line args=()
    while IFS= read -r line; do args+=("$line"); done < <(compose_files)
    (cd "$ROOT" && docker compose "${args[@]}" exec -T openflows-local-manager openflows-manager github-repos 2>/dev/null)
}
stage_connected_local_authorization() {
    [ "$(env_get OPENFLOWS_GITHUB_AUTH_MODE)" = connected-local ] || return 0
    TOTAL=$((TOTAL + 1))
    step "Connect GitHub"
    local manager_url="http://127.0.0.1:3002" status
    ( for _ in {1..30}; do curl -fs -m 3 "${manager_url}/health" >/dev/null 2>&1 && exit 0; sleep 1; done; exit 1 ) &
    spin $! "Waiting for the local OpenFlows console" \
        || die "The local OpenFlows console did not become ready." "Check logs: docker compose logs openflows-local-manager"
    status="$(curl -fs -m 5 "${manager_url}/api/v1/local/github" 2>/dev/null || true)"
    printf '%s' "$status" | python3 -c 'import json,sys; data=json.load(sys.stdin); sys.exit(0 if data.get("configured") else 1)' \
        || die "This OpenFlows distribution does not include the team GitHub App configuration." \
               "Use an official OpenFlows release or contact the deployment operator; contributors do not supply a client ID or App slug."
    if manager_repo_list | awk -F '\t' -v target="$REPO" 'BEGIN { target=tolower(target) } tolower($2)==target { found=1 } END { exit !found }'; then
        ok "GitHub user authorization ready for ${REPO}"
        return
    fi
    panel "$C" "Connect GitHub in your browser" \
        "Open the local console, install the OpenFlows App on ${REPO}, approve GitHub, then select ${REPO}." \
        "${manager_url}"
    local console_key
    console_key="$(<"$(env_get OPENFLOWS_CONNECTED_LOCAL_SECRET_DIR)/console-key")"
    open_url "${manager_url}/#console-key=${console_key}"
    is_tty || die "Complete GitHub connection in ${manager_url}, then re-run setup with a terminal attached."
    pause "Complete GitHub connection in the local console, then continue"
    manager_repo_list | awk -F '\t' -v target="$REPO" 'BEGIN { target=tolower(target) } tolower($2)==target { found=1 } END { exit !found }' \
        || die "${REPO} is not available to both your GitHub account and the OpenFlows App." \
               "Return to ${manager_url}, check the App installation and GitHub connection, then re-run setup."
    ok "GitHub user authorization ready for ${REPO}"
}
