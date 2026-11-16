# OpenFlows workspace runtime: the startup steps every role shares.
#
# The role templates source this file from their Coder startup script, check
# OF_RUNTIME_API, set their role's settings and call one bootstrap entry point:
# of_bootstrap_worker (FORGE, SENTINEL, VESSEL, LORE) or of_bootstrap_controller
# (NEXUS). Then they export the role's environment and start the role's own
# commands. The step order, and the rules it guarantees (readiness only after the
# checkout and instructions exist), live here once for every role and every
# workspace backend.
#
# The image installs this file at /opt/openflows/runtime/lib/bootstrap.sh. A copy
# mounted at /opt/openflows-dev/runtime/lib/bootstrap.sh (`make dev-sync`)
# replaces it for local development.
#
# Callers run under `set -e` and set:
#   OF_HOSTED           "true" when the workspace uses Manager-brokered credentials
#   OF_CONNECTED_LOCAL  "true" when git uses the connected-local credential helper
#
# Error convention: a step the workspace cannot work without ends startup with
# `of_fatal` (a "FATAL:" line and exit 1); a degraded but usable state logs a
# "WARNING:" line and continues.
#
# Secrets: tokens never reach the log or a process argument list. Request bodies
# go to curl on stdin, Authorization headers through a file descriptor, and JSON
# responses to python on stdin.
#
# Privilege: only of_prepare (ownership of the workspace paths), reading the
# root-owned runtime secret, and installing development programs use sudo. Every
# other step runs as the coder user, so a backend that grants ownership another
# way (a Kubernetes fsGroup or init container) needs no sudo after startup. A
# hosted workspace gives up sudo before it reports readiness
# (of_release_privileges), so nothing an agent runs there can become root and
# read the runtime secret.

# The contract between the role templates and this file. Templates refuse to
# start on a runtime with another API; change it only with the templates.
OF_RUNTIME_API=1

OF_RUNTIME_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
OF_RUNTIME_VERSION="$(cat "$OF_RUNTIME_DIR/VERSION" 2>/dev/null || echo unknown)"
OF_HOME=/home/coder
OF_WORKSPACE=/home/coder/workspace
OF_STATE_DIR=/home/coder/.local/state/openflows
OF_READY_FILE=/tmp/openflows-manager-ready
OF_API_TOKEN_FILE=/home/coder/.config/openflows/github-api-token
OF_GIT_CREDENTIALS=/home/coder/.git-credentials
OF_GOLDEN_REPO=/home/coder/.openflows/artifacts/repo
OF_BIN_DIR=/usr/local/bin
OF_DEV_DIR=/opt/openflows-dev
OF_SUDOERS_FILE=/etc/sudoers.d/coder
OF_CONNECTED_LOCAL_HELPER=/opt/openflows/connected-local-github.py

of_log() { echo "[$(date -u +%Y-%m-%dT%H:%M:%SZ)] $*" >&2; }
# Role commands in the templates log through the same format.
log() { of_log "$@"; }
of_fatal() {
    of_log "FATAL: $*"
    exit 1
}
of_hosted() { [ "${OF_HOSTED:-false}" = true ]; }
of_connected_local() { [ "${OF_CONNECTED_LOCAL:-false}" = true ]; }

# of_json_field FIELD — print FIELD of the JSON document on stdin.
of_json_field() {
    python3 -c 'import json,sys; print(json.load(sys.stdin)[sys.argv[1]])' "$1"
}

# of_curl_auth SCHEME TOKEN CURL_ARG... — curl with an Authorization header read
# from a file descriptor, so the token never appears in curl's argument list.
of_curl_auth() {
    local scheme="$1" token="$2"
    shift 2
    curl -H @<(printf 'Authorization: %s %s\n' "$scheme" "$token") "$@"
}

# of_prepare [--own-artifacts] — create the state directory and give the
# workspace paths to the coder user: the workspace volume (new volumes are
# root-owned; files a previous release wrote as root are handed back) and
# ~/.openflows, which Docker creates as root to hold the artifacts mount.
# --own-artifacts (Nexus) also takes the tenant artifacts volume it seeds.
of_prepare() {
    sudo mkdir -p "$OF_WORKSPACE" "$OF_HOME/.openflows"
    sudo chown coder:coder "$OF_HOME/.openflows"
    sudo find "$OF_WORKSPACE" -xdev \! -user coder -exec chown -h coder:coder {} +
    if [ "${1:-}" = --own-artifacts ]; then
        sudo chown -R coder:coder "$OF_HOME/.openflows"
    fi
    mkdir -p "$OF_STATE_DIR"
    of_log "Workspace runtime $OF_RUNTIME_VERSION (API $OF_RUNTIME_API) from $OF_RUNTIME_DIR"
}

# of_is_linux_amd64 FILE — true when FILE is a little-endian x86-64 ELF program.
# A program built for another system (a macOS build synced by mistake) would
# replace the image's copy and fail with cryptic errors when run.
of_is_linux_amd64() {
    [ "$(od -An -tx1 -N6 "$1" | tr -d ' \n')" = 7f454c460201 ] &&
        [ "$(od -An -tx1 -j18 -N2 "$1" | tr -d ' \n')" = 3e00 ]
}

# of_install_programs PROGRAM... — make each OpenFlows program available on PATH.
# The image ships them in /usr/local/bin. A development copy mounted in
# /opt/openflows-dev replaces the image's when it is a Linux x86-64 program.
of_install_programs() {
    local program dev target
    for program in "$@"; do
        target="$OF_BIN_DIR/$program"
        dev="$OF_DEV_DIR/$program"
        if [ -f "$dev" ]; then
            if of_is_linux_amd64 "$dev"; then
                sudo install -m 0755 "$dev" "$target"
                of_log "Using the mounted development copy of $program"
            else
                of_log "WARNING: $dev is not a Linux x86-64 program; keeping the image's $program. Run 'make dev-sync' on the host to rebuild it."
            fi
        fi
        [ -x "$target" ] || of_fatal "$program is not installed: the workspace image does not contain it and no development copy is mounted"
    done
}

# of_load_runtime_credentials [ENV=key ...] — export the values of the mounted
# runtime secret. The mount is root-owned, so it is read through sudo into a
# private copy that is removed straight after parsing. Every role exports the
# Manager URL, bootstrap token, Redis URL and tenant namespace; extra ENV=key
# pairs export more fields of the secret.
of_load_runtime_credentials() {
    local RUNTIME_CREDENTIAL_COPY exports
    [ -n "${OPENFLOWS_RUNTIME_CREDENTIAL_FILE:-}" ] || of_fatal "OPENFLOWS_RUNTIME_CREDENTIAL_FILE is not set"
    RUNTIME_CREDENTIAL_COPY=$(mktemp)
    chmod 600 "$RUNTIME_CREDENTIAL_COPY"
    if ! sudo cat "$OPENFLOWS_RUNTIME_CREDENTIAL_FILE" > "$RUNTIME_CREDENTIAL_COPY"; then
        rm -f "$RUNTIME_CREDENTIAL_COPY"
        of_fatal "runtime secret is not mounted"
    fi
    if ! exports=$(python3 - "$RUNTIME_CREDENTIAL_COPY" \
        OF_MANAGER_URL=manager_url OF_BOOTSTRAP=bootstrap_token \
        REDIS_URL=redis_url OPENFLOWS_TENANT=redis_namespace "$@" 2>/dev/null <<'PY'
import json, shlex, sys
d = json.load(open(sys.argv[1]))
for pair in sys.argv[2:]:
    env, key = pair.split("=", 1)
    print("export %s=%s" % (env, shlex.quote(str(d[key]))))
PY
    ); then
        rm -f "$RUNTIME_CREDENTIAL_COPY"
        of_fatal "runtime secret is unreadable or lacks a required field"
    fi
    rm -f "$RUNTIME_CREDENTIAL_COPY"
    eval "$exports"
}

# of_register_runtime — exchange the one-use bootstrap token for a runtime
# session, export OF_RUNTIME_ACCESS_TOKEN and OF_RUNTIME_REFRESH_TOKEN, and
# forget the spent bootstrap token.
of_register_runtime() {
    local session
    session=$(python3 -c 'import json,os; print(json.dumps({"bootstrap":os.environ["OF_BOOTSTRAP"]}))' |
        curl -fsS -H 'Content-Type: application/json' --data @- \
            "${OF_MANAGER_URL}/api/v1/runtime/register") ||
        of_fatal "Manager runtime registration failed. Stop and then start this tenant through OpenFlows to inject a fresh bootstrap; restarting directly in Coder reuses a single-use credential."
    OF_RUNTIME_ACCESS_TOKEN=$(printf '%s' "$session" | of_json_field access_token 2>/dev/null) &&
        OF_RUNTIME_REFRESH_TOKEN=$(printf '%s' "$session" | of_json_field refresh_token 2>/dev/null) &&
        [ -n "$OF_RUNTIME_ACCESS_TOKEN" ] && [ -n "$OF_RUNTIME_REFRESH_TOKEN" ] ||
        of_fatal "Manager runtime registration returned no session"
    export OF_RUNTIME_ACCESS_TOKEN OF_RUNTIME_REFRESH_TOKEN
    unset OF_BOOTSTRAP
}

# of_broker_token PURPOSE — print a short-lived GitHub token for PURPOSE (git or
# api) from the Manager broker. Call as VAR=$(of_broker_token git) || exit 1.
of_broker_token() {
    local response token
    response=$(printf '{"purpose":"%s"}' "$1" |
        of_curl_auth Bearer "$OF_RUNTIME_ACCESS_TOKEN" -fsS -H 'Content-Type: application/json' \
            --data @- "${OF_MANAGER_URL}/api/v1/runtime/github-credentials") ||
        of_fatal "Manager GitHub credential exchange failed after runtime registration (purpose: $1)"
    token=$(printf '%s' "$response" | of_json_field token 2>/dev/null) && [ -n "$token" ] ||
        of_fatal "Manager GitHub credential exchange returned no token (purpose: $1)"
    printf '%s\n' "$token"
}

# of_resolve_github_tokens [--api] — set GIT_TOKEN, and API_TOKEN with --api.
# Hosted workspaces get brokered installation tokens; local workspaces get the
# user token the local Manager issues through the connected-local helper.
of_resolve_github_tokens() {
    if of_hosted; then
        GIT_TOKEN=$(of_broker_token git) || exit 1
        if [ "${1:-}" = --api ]; then
            if of_connected_local; then
                API_TOKEN="$GIT_TOKEN"
            else
                API_TOKEN=$(of_broker_token api) || exit 1
            fi
        fi
    else
        python3 "$OF_CONNECTED_LOCAL_HELPER" setup || of_fatal "connected-local GitHub authorization failed"
        GIT_TOKEN=$(python3 "$OF_CONNECTED_LOCAL_HELPER" token) || of_fatal "connected-local GitHub token request failed"
        API_TOKEN="$GIT_TOKEN"
    fi
}

# of_write_git_credentials FILE TOKEN — store TOKEN for github.com, readable only by its owner.
of_write_git_credentials() {
    (umask 077 && printf 'https://x-access-token:%s@github.com\n' "$2" > "$1")
}

# of_store_git_credentials required|optional — configure the coder user's git to
# authenticate to GitHub. In connected-local mode the helper answers git directly
# and no token is stored on disk; otherwise GIT_TOKEN is stored. Without a token,
# `required` ends startup and `optional` continues with a warning.
of_store_git_credentials() {
    local policy="$1"
    if [ -z "${GIT_TOKEN:-}" ]; then
        [ "$policy" = optional ] || of_fatal "no GitHub credential is available for this workspace"
        of_log "WARNING: No GitHub token available — the workspace cannot clone or push until GitHub access is authorized"
    fi
    git config --global --unset-all credential.helper 2>/dev/null || true
    if of_connected_local; then
        git config --global credential.helper "!python3 $OF_CONNECTED_LOCAL_HELPER credential"
        rm -f "$OF_GIT_CREDENTIALS"
        of_log "Configured git to use the connected-local GitHub credential helper"
    elif [ -n "${GIT_TOKEN:-}" ]; then
        git config --global credential.helper store
        of_write_git_credentials "$OF_GIT_CREDENTIALS" "$GIT_TOKEN"
        of_log "Configured git credentials for GitHub"
    fi
}

# of_store_api_token — write API_TOKEN to the file the harness reads for pull
# request operations, replacing it atomically.
of_store_api_token() {
    local tmp
    [ -n "${API_TOKEN:-}" ] || of_fatal "no GitHub API credential for pull request operations"
    mkdir -p "$(dirname "$OF_API_TOKEN_FILE")"
    chmod 700 "$(dirname "$OF_API_TOKEN_FILE")"
    tmp=$(mktemp "$OF_API_TOKEN_FILE.XXXXXX")
    printf '%s' "$API_TOKEN" > "$tmp"
    mv -f "$tmp" "$OF_API_TOKEN_FILE"
}

# of_configure_git_identity required|optional — set the git author. Hosted
# installation tokens do not expose /user, so they commit as the App bot;
# otherwise the token's GitHub account is used, retrying transient GitHub
# failures. Without a token, `required` ends startup and `optional` continues
# with a warning (the next start sets the identity).
of_configure_git_identity() {
    local policy="$1" login="" attempt
    if of_hosted && ! of_connected_local; then
        login="openflows[bot]"
    elif [ -n "${GIT_TOKEN:-}" ]; then
        for attempt in 1 2 3 4 5; do
            login=$(of_curl_auth token "$GIT_TOKEN" -fsSL --max-time 20 \
                "${GITHUB_API_BASE:-https://api.github.com}/user" | of_json_field login 2>/dev/null) &&
                [ -n "$login" ] && break
            login=""
            of_log "GitHub identity lookup failed (attempt $attempt/5)"
            [ "$attempt" -eq 5 ] || sleep $((attempt * 5))
        done
        [ -n "$login" ] || of_fatal "could not resolve the GitHub user identity"
    fi
    if [ -z "$login" ]; then
        [ "$policy" = optional ] || of_fatal "no GitHub credential to resolve the git author identity"
        of_log "WARNING: no GitHub token yet — git author identity not configured; this workspace cannot push until GitHub access is authorized"
        return 0
    fi
    of_log "GitHub identity resolved: $login"
    # GitHub's noreply address keeps the account's email private.
    git config --global user.name "$login"
    git config --global user.email "$login@users.noreply.github.com"
    # Non-secret record of the account this workspace commits as. Kept out of
    # the checkout so it never blocks a clone into an empty workspace.
    echo "$login" > "$OF_HOME/.openflows-gh-identity" 2>/dev/null || true
}

# of_install_artifact_bundle — set ASSETS_DIR to the role assets (persona,
# skills, commands, standards, hooks). A hosted workspace fetches the bundle of
# its release from the Manager, which verifies its digest; a local workspace
# reads the tenant artifacts volume the Nexus workspace seeds.
of_install_artifact_bundle() {
    ASSETS_DIR="$OF_HOME/.openflows/artifacts"
    if of_hosted; then
        ASSETS_DIR="$OF_HOME/.openflows/bundle"
        mkdir -p "$ASSETS_DIR"
        of_log "Installing the workspace artifact bundle from the Manager"
        # The harness reads the access token from OF_RUNTIME_ACCESS_TOKEN, so it
        # never appears in a process argument.
        if ! openflows-harness artifacts fetch --dir "$ASSETS_DIR" \
            --manager-url "$OF_MANAGER_URL"; then
            of_fatal "cannot install the workspace artifact bundle"
        fi
    fi
}

# of_install_role_hooks ROLE — install ROLE's agent hooks from ASSETS_DIR and
# register them in the Claude Code settings. Session start reads the dispatch,
# tool-use guards enforce policy, and Stop refuses to end the session until the
# flow artifacts exist. Only events whose scripts exist are registered.
of_install_role_hooks() {
    local role="$1" source="$ASSETS_DIR/plugin/hooks/$1" hooks_dir="$OF_HOME/.openflows/hooks"
    # Replace the directory rather than copy over it, so a hook a later release
    # dropped is not registered again.
    rm -rf "$hooks_dir"
    if [ -d "$source" ]; then
        mkdir -p "$hooks_dir"
        cp -r "$source/." "$hooks_dir/"
        chmod +x "$hooks_dir"/*.sh 2>/dev/null || true
        of_log "Installed $role hooks from the artifact bundle"
    else
        of_log "WARNING: no hooks found for role $role at $source"
    fi
    mkdir -p "$OF_HOME/.claude"
    of_write_hook_settings "$hooks_dir" "$OF_HOME/.claude/settings.json"
}

# of_write_hook_settings HOOKS_DIR SETTINGS — replace the `hooks` section of a
# Claude Code settings file with the scripts present in HOOKS_DIR, keeping every
# other setting.
of_write_hook_settings() {
    python3 - "$1" "$2" <<'PY'
import json, os, sys
hooks_dir, settings_path = sys.argv[1], sys.argv[2]
def cmd(name):
    path = os.path.join(hooks_dir, name)
    return path if os.path.isfile(path) else None
event_map = {
    "SessionStart": [(None, "session_start.sh")],
    "PreToolUse": [("Bash", "pre_bash_guard.sh"),
                   ("Bash", "pre_bash_readonly_guard.sh"),
                   ("Write|Edit|MultiEdit", "pre_write_check.sh")],
    "PostToolUse": [("Write|Edit|MultiEdit", "post_write_lint.sh"),
                    ("Write|Edit|MultiEdit", "post_write_validate.sh")],
    "PreCompact": [(None, "pre_compact_handoff.sh")],
    "Stop": [(None, "stop_require_artifact.sh"),
             (None, "stop_require_eval.sh")],
    "SubagentStop": [(None, "subagent_stop.sh")],
}
hooks = {}
for event, entries in event_map.items():
    matchers = []
    for matcher, script in entries:
        path = cmd(script)
        if not path:
            continue
        entry = {"hooks": [{"type": "command", "command": path}]}
        if matcher:
            entry["matcher"] = matcher
        matchers.append(entry)
    if matchers:
        hooks[event] = matchers
settings = {}
if os.path.exists(settings_path):
    try:
        settings = json.load(open(settings_path))
    except Exception:
        settings = {}
settings["hooks"] = hooks
json.dump(settings, open(settings_path, "w"), indent=2)
print(f"wrote {settings_path} with {len(hooks)} hook events", file=sys.stderr)
PY
}

# of_clear_workspace — empty the workspace before a clone. Only called when it
# holds no checkout, so nothing of value is lost.
of_clear_workspace() {
    find "$OF_WORKSPACE" -mindepth 1 -maxdepth 1 -exec rm -rf {} + 2>/dev/null || true
}

# of_acquire_repository REPO_URL [--checkout-default] — make the workspace a
# checkout of REPO_URL. An existing checkout is refreshed. Otherwise the golden
# seed on the tenant artifacts volume is copied (one network fetch per tenant,
# not per workspace) and refreshed; when there is no seed or the copy fails, the
# repository is cloned directly. Network steps are time-boxed; failures are
# logged loudly and the caller's of_require_repository decides whether startup
# can continue. --checkout-default leaves a new checkout on the remote's
# default head.
of_acquire_repository() {
    local repo_url="$1" checkout_default="${2:-}" fresh=""
    if [ -d "$OF_WORKSPACE/.git" ]; then
        cd "$OF_WORKSPACE"
        # A checkout left on the detached default head has nothing to pull into.
        if git symbolic-ref -q HEAD >/dev/null; then
            timeout -k 5s 45s git pull 2>/dev/null || of_log "WARNING: repository pull failed or timed out; using local checkout"
        fi
        timeout -k 5s 45s git fetch --all --prune 2>/dev/null || of_log "WARNING: repository fetch failed or timed out; using local refs"
        return 0
    fi
    if [ -d "$OF_GOLDEN_REPO/.git" ]; then
        if cp -a "$OF_GOLDEN_REPO/." "$OF_WORKSPACE/" 2>"$OF_STATE_DIR/golden-copy.log"; then
            of_log "Copied golden repo seed into $OF_WORKSPACE"
            fresh=1
        else
            of_log "WARNING: copying the golden repo seed failed — $(tail -5 "$OF_STATE_DIR/golden-copy.log" 2>/dev/null); falling back to a direct clone"
            of_clear_workspace
        fi
    elif [ -n "$repo_url" ]; then
        of_log "WARNING: no golden repo seed at $OF_GOLDEN_REPO — falling back to direct clone"
    fi
    if [ -z "$fresh" ]; then
        if [ -z "$repo_url" ]; then
            of_log "WARNING: no repo_url and no usable golden repo seed — workspace has no repository"
            return 0
        fi
        if timeout -k 5s 90s git clone "$repo_url" "$OF_WORKSPACE" 2>"$OF_STATE_DIR/clone.log"; then
            of_log "Cloned repository into $OF_WORKSPACE"
            fresh=1
        else
            of_log "WARNING: git clone failed — $(tail -5 "$OF_STATE_DIR/clone.log" 2>/dev/null)"
            return 0
        fi
    fi
    # Refresh before checkout (copy -> refresh -> checkout -> work).
    cd "$OF_WORKSPACE"
    timeout -k 5s 45s git fetch --all --prune 2>/dev/null || of_log "WARNING: repository fetch failed or timed out; using seed refs"
    if [ "$checkout_default" = --checkout-default ]; then
        git checkout origin/HEAD 2>/dev/null || git checkout main 2>/dev/null || true
    fi
}

# of_warn_branch_mismatch INTENDED — record that the workspace is not on the
# intended PR branch, so the rework flow does not push the fix to the wrong
# branch. The agent stays available; the marker and the log make it visible.
of_warn_branch_mismatch() {
    local intended="$1" current
    current="$(git rev-parse --abbrev-ref HEAD 2>/dev/null || echo unknown)"
    of_log "WARNING: workspace is on branch '$current', NOT the intended PR branch '$intended' — rework must NOT push to the current branch"
    # Best effort: a failed write must never end startup; the log carries the message.
    {
        echo "# REWORK BRANCH MISMATCH"
        echo ""
        echo "The startup script could not switch to the intended PR branch \`$intended\`."
        echo "The workspace is currently on branch \`$current\`, which is NOT the PR branch."
        echo ""
        echo "**Do NOT run \`git push\` on the current branch** — it is not the PR branch and"
        echo "pushing here would update the wrong branch, not the PR."
        echo "Resolve the local conflict or uncommitted changes and switch to \`$intended\`"
        echo "before performing any push."
    } > "$OF_WORKSPACE/REWORK_BRANCH_MISMATCH.md" || true
}

# of_checkout_assignment_branch TARGET_BRANCH WORKER TICKET — put the checkout on
# the branch this assignment works on. With TARGET_BRANCH (rework of an existing
# PR) resume that branch so completed work is never redone; otherwise create or
# resume WORKER/TICKET from the default head.
of_checkout_assignment_branch() {
    local target="$1" worker="$2" ticket="$3" work_branch
    [ -d "$OF_WORKSPACE/.git" ] || return 0
    cd "$OF_WORKSPACE"
    if [ -n "$target" ]; then
        # Resume on a NAMED branch (never detached), so the CI-fix and
        # /address_review flows can push with plain `git push`. Prefer an
        # existing local branch to keep in-flight work, then track origin. When
        # the branch exists in neither place (e.g. a fork-backed PR), stay on the
        # default branch WITHOUT creating a branch of that name at origin/HEAD,
        # which would start rework without the PR's commits.
        if git rev-parse --verify --quiet "refs/heads/$target" >/dev/null; then
            # Uncommitted work or an unresolved merge can block the switch; keep
            # the agent available and flag the mismatch instead of failing.
            if git checkout "$target" 2>/dev/null; then
                of_log "Checked out existing local branch: $target"
            else
                of_warn_branch_mismatch "$target"
            fi
        elif git checkout -B "$target" --track "origin/$target" 2>/dev/null; then
            of_log "Checked out target branch: $target"
        else
            git checkout origin/HEAD 2>/dev/null || git checkout main 2>/dev/null || true
            of_warn_branch_mismatch "$target"
        fi
    else
        work_branch="$worker/$ticket"
        if [ -z "$ticket" ] || ! git check-ref-format --branch "$work_branch" >/dev/null 2>&1; then
            of_fatal "cannot derive the worker/ticket branch from '$work_branch'"
        fi
        if git show-ref --verify --quiet "refs/heads/$work_branch"; then
            git checkout "$work_branch" || {
                of_warn_branch_mismatch "$work_branch"
                exit 1
            }
        elif git show-ref --verify --quiet "refs/remotes/origin/$work_branch"; then
            git checkout --track -b "$work_branch" "origin/$work_branch"
        else
            git checkout --no-track -b "$work_branch" origin/HEAD || git checkout --no-track -b "$work_branch"
        fi
        of_log "Assignment branch ready: $work_branch"
    fi
}

# of_require_repository — a worker without a checkout cannot do useful work.
of_require_repository() {
    [ -d "$OF_WORKSPACE/.git" ] || of_fatal "repository bootstrap did not produce $OF_WORKSPACE/.git"
}

# of_install_role_instructions ROLE — install ROLE's persona, skills, commands
# and standards from ASSETS_DIR into the checkout, and exclude them from git
# locally so they never appear in the role's commits.
of_install_role_instructions() {
    local role="$1" root="$ASSETS_DIR/orchestration" persona manifest skill standard exclude pattern
    persona="$root/agent/agents/$role.agent.md"
    manifest="$root/agent/roles/$role.skills"
    [ -f "$persona" ] || of_fatal "hosted instruction bundle is missing $persona"
    [ -f "$manifest" ] || of_fatal "hosted skill manifest is missing $manifest"
    cp "$persona" "$OF_WORKSPACE/AGENTS.md"
    cp "$persona" "$OF_WORKSPACE/$role.agent.md"
    mkdir -p "$OF_WORKSPACE/.agents/skills" "$OF_WORKSPACE/.agents/commands"
    while IFS= read -r skill; do
        [ -z "$skill" ] && continue
        mkdir -p "$OF_WORKSPACE/.agents/skills/$skill"
        cp "$root/plugin/skills/$skill/SKILL.md" "$OF_WORKSPACE/.agents/skills/$skill/SKILL.md"
    done < "$manifest"
    cp -R "$root/plugin/commands/." "$OF_WORKSPACE/.agents/commands/"
    for standard in CODING.md SECURITY.md REVIEW.md; do
        [ ! -f "$root/agent/standards/$standard" ] || cp "$root/agent/standards/$standard" "$OF_WORKSPACE/"
    done
    exclude=$(git -C "$OF_WORKSPACE" rev-parse --absolute-git-dir)/info/exclude
    mkdir -p "$(dirname "$exclude")"
    touch "$exclude"
    for pattern in /AGENTS.md "/$role.agent.md" '/.agents/skills/' '/.agents/commands/' /CODING.md /SECURITY.md /REVIEW.md; do
        grep -qxF -- "$pattern" "$exclude" || printf '\n%s\n' "$pattern" >> "$exclude"
    done
    of_log "Installed hosted instructions for $role"
}

# of_start_credential_refresh [--api-token] — hosted only: keep the runtime
# session and the stored GitHub tokens fresh for the workspace's lifetime, and
# report liveness once of_signal_manager_ready has run.
of_start_credential_refresh() {
    of_hosted || return 0
    rm -f "$OF_READY_FILE"
    OF_CONNECTED_LOCAL="${OF_CONNECTED_LOCAL:-false}" \
        nohup "$OF_RUNTIME_DIR/bin/openflows-credential-refresh" "$@" >>"$OF_STATE_DIR/git-refresh.log" 2>&1 &
}

# of_signal_manager_ready — hosted only: tell the Manager this workspace has
# finished bootstrap. The Manager assigns work only after this heartbeat, so it
# must be the last bootstrap step before the role's own commands.
of_signal_manager_ready() {
    of_hosted || return 0
    of_curl_auth Bearer "$OF_RUNTIME_ACCESS_TOKEN" -fsS -X POST \
        "${OF_MANAGER_URL}/api/v1/runtime/heartbeat" >/dev/null ||
        of_fatal "the Manager did not accept the readiness heartbeat"
    of_log "Manager readiness heartbeat sent after bootstrap"
    touch "$OF_READY_FILE"
}

# of_release_privileges — hosted only: remove the coder user's sudo rule once
# the privileged steps are done, and prove sudo is gone. Agents in a hosted
# workspace then run without root, so they cannot read the root-owned runtime
# secret or tamper with the image. The image restores the rule on the next
# start, where startup needs it again. Local workspaces keep sudo so agents can
# install tools on the operator's own machine.
of_release_privileges() {
    of_hosted || return 0
    sudo rm -f "$OF_SUDOERS_FILE"
    if sudo -n true 2>/dev/null; then
        of_fatal "sudo is still available after removing $OF_SUDOERS_FILE; refusing to hand a root-capable workspace to an agent"
    fi
    of_log "Released sudo: the role's commands run without root"
}

# of_drop_runtime_session — forget the runtime session once bootstrap is done.
# The refresh loop holds the live tokens, so the role's commands never inherit
# a long-lived credential they do not use.
of_drop_runtime_session() {
    unset OF_BOOTSTRAP OF_RUNTIME_ACCESS_TOKEN OF_RUNTIME_REFRESH_TOKEN
}

# of_bootstrap_worker — the startup of every worker role, in order. Settings:
#   OF_ROLE              base role (forge, sentinel, vessel, lore); required
#   OF_REPO_URL          repository to clone when there is no checkout or seed
#   OF_GITHUB_ACCESS     required|optional (default): GitHub credentials for git
#   OF_GITHUB_API_TOKEN  true: also store the API-purpose token for pull requests
#   OF_GIT_IDENTITY      required|optional|none (default): git author identity
#   OF_ROLE_HOOKS        true: install the role's agent hooks
#   OF_CHECKOUT          default (default): stay on the default head;
#                        assignment: resume OF_TARGET_BRANCH, else OF_WORKER/OF_TICKET
# Readiness is reported only after the checkout and the role instructions exist.
of_bootstrap_worker() {
    local api="" identity="${OF_GIT_IDENTITY:-none}"
    [ -n "${OF_ROLE:-}" ] || of_fatal "OF_ROLE is not set"
    [ "${OF_GITHUB_API_TOKEN:-false}" != true ] || api=--api
    of_prepare
    of_install_programs openflows-harness
    if of_hosted; then
        of_load_runtime_credentials
        of_register_runtime
    fi
    of_resolve_github_tokens $api
    of_store_git_credentials "${OF_GITHUB_ACCESS:-optional}"
    [ -z "$api" ] || of_store_api_token
    [ "$identity" = none ] || of_configure_git_identity "$identity"
    of_install_artifact_bundle
    [ "${OF_ROLE_HOOKS:-false}" != true ] || of_install_role_hooks "$OF_ROLE"
    case "${OF_CHECKOUT:-default}" in
        assignment)
            of_acquire_repository "${OF_REPO_URL:-}"
            of_checkout_assignment_branch "${OF_TARGET_BRANCH:-}" "${OF_WORKER:-}" "${OF_TICKET:-}"
            ;;
        default) of_acquire_repository "${OF_REPO_URL:-}" --checkout-default ;;
        *) of_fatal "unknown OF_CHECKOUT '${OF_CHECKOUT}'" ;;
    esac
    of_start_credential_refresh ${api:+--api-token}
    of_require_repository
    of_install_role_instructions "$OF_ROLE"
    of_release_privileges
    of_signal_manager_ready
    of_drop_runtime_session
}

# of_bootstrap_controller [ENV=key ...] — the startup of the Nexus controller
# workspace. OF_REPO_URL names the controller's repository; ENV=key pairs export
# more fields of the hosted runtime secret. The controller keeps its runtime
# session (it calls the Manager itself) and reports its own readiness after
# opening its tenant store.
of_bootstrap_controller() {
    of_prepare --own-artifacts
    of_install_programs openflows openflows-harness
    if of_hosted; then
        of_load_runtime_credentials "$@"
        of_register_runtime
    fi
    of_resolve_github_tokens
    of_store_git_credentials required
    of_sync_controller_repository "${OF_REPO_URL:-}"
    # Local mode only: a hosted worker clones with its own brokered credential,
    # so there is no shared volume to seed.
    of_hosted || of_refresh_golden_seed "${OF_REPO_URL:-}"
    of_release_privileges
}

# of_sync_controller_repository REPO_URL — Nexus: refresh or clone the
# controller's own checkout. Failures are tolerated; the controller reports them.
of_sync_controller_repository() {
    if [ -d "$OF_WORKSPACE/.git" ]; then
        (cd "$OF_WORKSPACE" && timeout -k 5s 45s git pull 2>/dev/null) ||
            of_log "WARNING: controller repository pull failed or timed out; using local checkout"
    elif [ -n "$1" ]; then
        timeout -k 5s 90s git clone "$1" "$OF_WORKSPACE" 2>/dev/null ||
            of_log "WARNING: controller repository clone failed or timed out"
    fi
}

# of_refresh_golden_seed REPO_URL — Nexus, local mode: keep the golden checkout
# on the tenant artifacts volume current, so workers seed from an offline copy
# instead of each cloning over the network. Nexus holds the credentials and
# egress, so the expensive fetch happens once per tenant.
of_refresh_golden_seed() {
    [ -n "$1" ] || return 0
    if [ -d "$OF_GOLDEN_REPO/.git" ]; then
        timeout -k 5s 45s git -C "$OF_GOLDEN_REPO" fetch --prune 2>/dev/null ||
            of_log "WARNING: golden repo fetch failed or timed out; workers seed from its current refs"
        git -C "$OF_GOLDEN_REPO" reset --hard origin/HEAD 2>/dev/null || git -C "$OF_GOLDEN_REPO" reset --hard 2>/dev/null || true
        of_log "Refreshed golden repo at $OF_GOLDEN_REPO"
    else
        mkdir -p "$(dirname "$OF_GOLDEN_REPO")"
        if timeout -k 5s 90s git clone "$1" "$OF_GOLDEN_REPO" 2>/dev/null; then
            of_log "Seeded golden repo at $OF_GOLDEN_REPO"
        else
            of_log "WARNING: could not seed golden repo at $OF_GOLDEN_REPO — workers will not get an offline repo seed"
        fi
    fi
}
