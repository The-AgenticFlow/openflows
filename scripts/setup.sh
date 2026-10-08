#!/usr/bin/env bash
# OpenFlows one-command setup.
#
#   ./scripts/setup.sh [owner/repo] [--name TEAM] [--fleet N]   (N = FORGE-SENTINEL pairs; asked when omitted)
#
# Idempotent: every stage checks the real state first and skips if already
# done, so re-running after fixing a blocker resumes where it stopped.
# Stages: preflight → .env → GitHub App → Docker → Coder admin/token →
#         LLM model → bootstrap → tenant → doctor.
#
# Non-interactive use: pass owner/repo, and set OPENFLOWS_LLM_PROVIDER /
# OPENFLOWS_LLM_API_KEY / OPENFLOWS_LLM_MODEL when no LLM is configured yet.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
ENV_FILE="${ROOT}/.env"
TOTAL=8
STEP=0

# ── output helpers ──────────────────────────────────────────────────────────
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
    B=$'\033[1m'; D=$'\033[2m'; G=$'\033[32m'; Y=$'\033[33m'; R=$'\033[31m'; C=$'\033[36m'; N=$'\033[0m'
else
    B=; D=; G=; Y=; R=; C=; N=
fi
if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then FANCY=1; else FANCY=0; fi   # animations / cursor control only when 1
LOG_FILE="${ROOT}/.setup.log"
START_TIME=$SECONDS
RAIL_OPEN=0
STEP_START=0
PW=66   # panel inner width

# Left "rail": every line inside a step is prefixed with │ so steps read as framed sections.
rail() { if [ "$RAIL_OPEN" = 1 ]; then printf '%s│%s ' "$D" "$N"; fi; }
dots() { # dots DONE TOTAL → ●●●○○○
    local i out=
    for ((i = 1; i <= $2; i++)); do if [ "$i" -le "$1" ]; then out+="●"; else out+="○"; fi; done
    printf '%s' "$out"
}
close_step() {
    if [ "$RAIL_OPEN" = 1 ]; then
        printf '%s└─ %ds%s\n' "$D" $((SECONDS - STEP_START)) "$N"
        RAIL_OPEN=0
    fi
}
step() {
    close_step
    STEP=$((STEP + 1)); STEP_START=$SECONDS
    printf '\n%s┌─%s %s%s%s  %s%s%s %s·%s %s%s%s\n' "$D" "$N" "$C" "$(dots "$STEP" "$TOTAL")" "$N" "$D" "Step $STEP of $TOTAL" "$N" "$D" "$N" "$B" "$1" "$N"
    RAIL_OPEN=1
}
ok()   { rail; printf '%s✓%s %s\n' "$G" "$N" "$1"; }
info() { rail; printf '  %s%s%s\n' "$D" "$1" "$N"; }
warn() { rail; printf '%s!%s %s\n' "$Y" "$N" "$1"; }

# panel COLOR TITLE LINE… — rounded box; lines are plain text (padded by characters, not bytes)
panel() {
    local color=$1 title=$2; shift 2
    local w=$PW line pad rule
    for line in "$@"; do [ "${#line}" -gt "$w" ] && w=${#line}; done
    rule="$(printf '─%.0s' $(seq $((w + 2))))"
    rail; printf '%s╭─ %s%s%s %s%s╮%s\n' "$color" "$B" "$title" "$N$color" "$(printf '─%.0s' $(seq $((w - ${#title} - 1 > 0 ? w - ${#title} - 1 : 1))))" "" "$N"
    for line in "$@"; do
        pad="$line"; while [ "${#pad}" -lt "$w" ]; do pad+=" "; done
        rail; printf '%s│%s %s %s│%s\n' "$color" "$N" "$pad" "$color" "$N"
    done
    rail; printf '%s╰%s╯%s\n' "$color" "$rule" "$N"
}
die() {
    local title=$1; shift
    close_step
    printf '\n' >&2
    { RAIL_OPEN=0; panel "$R" "✗ ${title}" "" "$@" ""; } >&2
    printf '\n  %sFix the above, then re-run: ./scripts/setup.sh%s\n\n' "$D" "$N" >&2
    exit 1
}
banner() {
    local w=$((PW)) line text
    local rule; rule="$(printf '─%.0s' $(seq $((w + 2))))"
    printf '\n%s╭%s╮%s\n' "$C" "$rule" "$N"
    for text in "" "OpenFlows" "Turn GitHub issues into reviewed pull requests" ""; do
        line="$text"; while [ "${#line}" -lt "$w" ]; do line+=" "; done
        case "$text" in
            OpenFlows) printf '%s│%s %s%s%s %s│%s\n' "$C" "$N" "$B" "$line" "$N" "$C" "$N" ;;
            *)         printf '%s│%s %s%s%s %s│%s\n' "$C" "$N" "$D" "$line" "$N" "$C" "$N" ;;
        esac
    done
    printf '%s╰%s╯%s\n' "$C" "$rule" "$N"
}
# spin PID "label" — animate while PID runs (plain line when not a terminal)
spin() {
    local pid=$1 label=$2 frames='⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏' i=0 start=$SECONDS
    if [ "$FANCY" = 1 ]; then
        while kill -0 "$pid" 2>/dev/null; do
            if [ -n "${SPIN_STATUS_FN:-}" ] && [ $((i % 5)) -eq 0 ]; then label="$($SPIN_STATUS_FN)"; fi
            printf '\r%s%s%s %s%s%s %s%ds%s\033[K' "$(rail)" "$C" "${frames:i++%${#frames}:1}" "$N" "$label" "$N" "$D" "$((SECONDS - start))" "$N"
            sleep 0.1
        done
        printf '\r\033[K'
    else
        printf '  %s…\n' "$label"
    fi
    wait "$pid"
}
# menu "Title" ITEM… — arrow-key / j-k / number selector; prints the chosen 0-based index on stdout (UI on stderr)
menu() {
    local title=$1; shift
    local items=("$@") n=$# sel=0 i key rest
    if [ "$FANCY" != 1 ]; then   # plain mode (NO_COLOR / no TTY): numbered prompt, no escape codes
        printf '%s?%s %s\n' "$Y" "$N" "$title" >&2
        for ((i = 0; i < n; i++)); do printf '    %d) %s\n' $((i + 1)) "${items[i]}" >&2; done
        printf '  Choice [1]: ' >&2
        read -r key || key=""
        if [[ "$key" =~ ^[0-9]+$ ]] && [ "$key" -ge 1 ] && [ "$key" -le "$n" ]; then printf '%s' $((key - 1)); else printf '0'; fi
        return
    fi
    printf '%s' "$(rail)" >&2; printf '%s?%s %s%s%s\n' "$Y" "$N" "$B" "$title" "$N" >&2
    printf '\033[?25l' >&2
    while true; do
        for ((i = 0; i < n; i++)); do
            printf '%s' "$(rail)" >&2
            if [ "$i" -eq "$sel" ]; then printf '  %s❯ %s%s\033[K\n' "$C$B" "${items[i]}" "$N" >&2
            else printf '    %s%s%s\033[K\n' "$D" "${items[i]}" "$N" >&2; fi
        done
        printf '%s' "$(rail)" >&2; printf '  %s↑/↓ move · Enter select%s\033[K' "$D" "$N" >&2
        IFS= read -rsn1 key || key=""
        if [ "$key" = $'\033' ]; then read -rsn2 -t 1 rest || rest=""; key="$rest"; fi
        case "$key" in
            '[A'|k) sel=$(( (sel + n - 1) % n )) ;;
            '[B'|j) sel=$(( (sel + 1) % n )) ;;
            [1-9]) [ "$key" -le "$n" ] && sel=$((key - 1)) ;;
            "") break ;;
        esac
        printf '\r\033[%dA' "$n" >&2
    done
    printf '\r\033[%dA\033[J' "$n" >&2          # collapse the menu to one summary line
    printf '\033[1A\033[J' >&2
    printf '%s' "$(rail)" >&2; printf '%s✓%s %s%s%s  %s%s%s\n' "$G" "$N" "$D" "$title" "$N" "$B" "${items[sel]}" "$N" >&2
    printf '\033[?25h' >&2
    printf '%s' "$sel"
}
pause() { # pause "message" — wait for Enter
    printf '%s' "$(rail)" >&2; printf '%s⏎%s  %s %s(press Enter)%s ' "$Y" "$N" "$1" "$D" "$N" >&2
    read -r _ || true
}
# run_quiet "label" cmd… — run with a spinner, full output to .setup.log; show the tail on failure
run_quiet() {
    local label=$1; shift
    : >"$LOG_FILE"
    "$@" >"$LOG_FILE" 2>&1 &
    if ! spin $! "$label"; then
        printf '\n' >&2
        tail -n 25 "$LOG_FILE" | sed 's/\x1b\[[0-9;]*m//g; s/^/    /' >&2
        die "${label} failed" "Full log: ${LOG_FILE}"
    fi
}
is_tty() { [ -t 0 ] && [ -t 1 ]; }
have() { command -v "$1" >/dev/null 2>&1; }

ask() { # ask "Prompt" "default" → stdout
    local prompt="$1" default="${2:-}" reply
    printf '%s' "$(rail)" >&2
    if [ -n "$default" ]; then printf '%s?%s %s %s(%s)%s ' "$Y" "$N" "$prompt" "$D" "$default" "$N" >&2; else printf '%s?%s %s ' "$Y" "$N" "$prompt" >&2; fi
    read -r reply || true
    printf '%s' "${reply:-$default}"
}
ask_secret() {
    local reply
    printf '%s' "$(rail)" >&2
    printf '%s?%s %s %s(input hidden)%s ' "$Y" "$N" "$1" "$D" "$N" >&2
    read -rs reply || true
    printf '\n' >&2
    printf '%s' "$reply"
}
open_url() {
    if have xdg-open; then xdg-open "$1" >/dev/null 2>&1 &
    elif have open; then open "$1" >/dev/null 2>&1 &
    fi
    return 0
}
rand_hex() { od -An -N"$1" -tx1 /dev/urandom | tr -d ' \n'; }
json() { python3 -c 'import sys,json
try:
    d=json.load(sys.stdin)
    for k in sys.argv[1].split("."):
        d=d[int(k)] if isinstance(d,list) else d[k]
    print(d if not isinstance(d,(dict,list)) else json.dumps(d))
except Exception:
    pass' "$1"; }

# ── .env helpers ────────────────────────────────────────────────────────────
env_get() { [ -f "$ENV_FILE" ] && sed -n "s/^$1=//p" "$ENV_FILE" | tail -1 || true; }
env_set() { # env_set KEY VALUE — replace the line or append
    local key="$1" val="$2" tmp
    tmp="$(mktemp)"
    if [ -f "$ENV_FILE" ] && grep -q "^${key}=" "$ENV_FILE"; then
        KEY="$key" VAL="$val" awk -F= '$1==ENVIRON["KEY"]{print ENVIRON["KEY"] "=" ENVIRON["VAL"]; next} {print}' "$ENV_FILE" >"$tmp"
    else
        { [ -f "$ENV_FILE" ] && cat "$ENV_FILE"; printf '%s=%s\n' "$key" "$val"; } >"$tmp"
    fi
    cat "$tmp" >"$ENV_FILE" && rm -f "$tmp"
    chmod 600 "$ENV_FILE"
}
is_placeholder() { case "$1" in ""|your_*|replace_with*|*"<"*) return 0 ;; *) return 1 ;; esac; }

coder_port() { local p; p="$(env_get CODER_PORT)"; printf '%s' "${p:-7080}"; }
auth_id() { local i; i="$(env_get CODER_EXTERNAL_AUTH_0_ID)"; printf '%s' "${i:-primary-github}"; }
# URL a browser should use (CODER_ACCESS_URL when set, else the local URL)
public_url() { local u; u="$(env_get CODER_ACCESS_URL)"; printf '%s' "${u:-$(coder_url)}"; }
coder_url() { local u; u="$(env_get CODER_URL)"; printf '%s' "${u:-http://localhost:$(coder_port)}"; }
api() { # api METHOD PATH [JSON] — uses CODER_TOKEN; prints body; on HTTP error prints Coder's message to stderr
    local method="$1" path="$2" body="${3:-}" out code
    local args=(-sS -m 30 -X "$method" -H "Coder-Session-Token: ${CODER_TOKEN:-}" -w $'\n%{http_code}')
    [ -n "$body" ] && args+=(-H 'Content-Type: application/json' -d "$body")
    out="$(curl "${args[@]}" "$(coder_url)${path}")" || return 1
    code="${out##*$'\n'}"; out="${out%$'\n'*}"
    if [ "${code:0:1}" != "2" ]; then
        printf '    Coder said (%s): %s\n' "$code" "$(printf '%s' "$out" | json detail | grep . || printf '%s' "$out" | json message)" >&2
        return 1
    fi
    printf '%s' "$out"
}
port_busy() { (exec 3<>"/dev/tcp/127.0.0.1/$1") 2>/dev/null; }

# ── stage 1: preflight ──────────────────────────────────────────────────────
# clean_installer_log FILE — installer output without progress bars, ANSI codes or shell-trace (+ …) lines
clean_installer_log() {
    tr '\r' '\n' <"$1" | sed 's/\x1b\[[0-9;]*[a-zA-Z]//g' \
        | grep -Ev '^[[:space:]]*[#=O -]*[0-9.]*%?[[:space:]]*$|^\+ |^[[:space:]]*$' || true
}
# status line for the spinner: latest meaningful installer line + download percent
coder_install_status() {
    local last pct
    last="$(clean_installer_log "$LOG_FILE" | tail -n 1 | cut -c1-60)"
    pct="$(tr '\r' '\n' <"$LOG_FILE" | grep -Eo '[0-9]+(\.[0-9])?%' | tail -n 1)"
    printf 'Installing the coder CLI%s%s' "${last:+ · $last}" "${pct:+ · $pct}"
}
# install_coder PREFIX — run Coder's official installer behind a live status line; show what it did afterwards
install_coder() {
    local prefix="$1" rc
    : >"$LOG_FILE"
    curl -fsSL https://coder.com/install.sh 2>>"$LOG_FILE" | sh -s -- --method standalone --prefix "$prefix" >>"$LOG_FILE" 2>&1 &
    SPIN_STATUS_FN=coder_install_status spin $! "Installing the coder CLI (downloading ~100 MB)"
    rc=$?
    clean_installer_log "$LOG_FILE" | while IFS= read -r line; do info "$line"; done
    [ "$rc" -eq 0 ] && [ -x "${prefix}/bin/coder" ]
}

stage_preflight() {
    step "Checking your machine"
    local missing=()
    have curl    || missing+=("curl      → install with your package manager")
    have python3 || missing+=("python3   → install with your package manager")
    have cargo   || missing+=("cargo     → curl https://sh.rustup.rs -sSf | sh")
    if ! have docker; then
        missing+=("docker    → https://docs.docker.com/get-docker/")
    elif ! docker info >/dev/null 2>&1; then
        missing+=("docker daemon is not running → start Docker (or: sudo systemctl start docker)")
    elif ! docker compose version >/dev/null 2>&1; then
        missing+=("docker compose v2 plugin → https://docs.docker.com/compose/install/")
    fi
    if [ "${#missing[@]}" -gt 0 ]; then
        die "Missing requirements:" "${missing[@]/#/- }" "Install them, then:"
    fi
    if ! have coder; then
        local cmd='curl -fsSL https://coder.com/install.sh | sh -s -- --method standalone --prefix "$HOME/.local"'
        warn "The 'coder' CLI is not installed"
        if is_tty && [[ "$(ask "Install it now? (runs the official installer from coder.com)" "Y")" =~ ^[Yy] ]]; then
            install_coder "$HOME/.local" || die "Could not install the coder CLI." "Run it yourself:  ${cmd}" "Installer output: ${LOG_FILE}"
            export PATH="$HOME/.local/bin:$PATH"
            have coder || die "coder CLI installed but not on PATH." "Add ~/.local/bin to PATH"
        else
            die "The coder CLI is required." "Install it:  ${cmd}"
        fi
    fi
    # Ports only matter when our own stack is not already up.
    if [ -z "$(cd "$ROOT" && docker compose ps -q coder 2>/dev/null)" ]; then
        local port
        for port in "$(coder_port)" 6379; do
            if port_busy "$port"; then
                die "Port $port is already in use by something else." \
                    "Find it:  docker ps --filter publish=$port   (or: ss -ltnp | grep :$port)" \
                    "Stop it, or (for Coder) set CODER_PORT in .env to a free port."
            fi
        done
    fi
    ok "docker, compose, cargo, coder, python3, curl"
}

# ── stage 2: .env ───────────────────────────────────────────────────────────
stage_env() {
    step "Preparing .env"
    if [ ! -f "$ENV_FILE" ]; then
        cp "${ROOT}/.env.example" "$ENV_FILE"
        chmod 600 "$ENV_FILE"
        ok "created .env from .env.example"
    fi
    if is_placeholder "$(env_get CODER_CHAT_HOOK_SECRET)"; then
        env_set CODER_CHAT_HOOK_SECRET "$(rand_hex 32)"
        ok "generated CODER_CHAT_HOOK_SECRET"
    fi
    if is_placeholder "$(env_get CODER_SESSION_TOKEN)"; then
        env_set CODER_SESSION_TOKEN ""
    fi
    ok ".env ready"
}

# ── stage 3: GitHub App ─────────────────────────────────────────────────────
github_app_via_manifest() { # $1=owner → sets client id/secret/install url in .env
    local owner="$1" owner_type="User" port state name target manifest_file out_file code
    owner_type="$(curl -fsS -m 10 "https://api.github.com/users/${owner}" 2>/dev/null | json type || true)"
    if [ "$owner_type" = "Organization" ]; then
        target="https://github.com/organizations/${owner}/settings/apps/new"
    else
        target="https://github.com/settings/apps/new"
    fi
    state="$(rand_hex 16)"
    name="OpenFlows-${owner}-$(rand_hex 2)"
    out_file="$(mktemp)"
    port="$(python3 -c 'import socket;s=socket.socket();s.bind(("127.0.0.1",0));print(s.getsockname()[1])')"
    manifest_file="$(mktemp)"
    NAME="$name" PORT="$port" CB="$(public_url)/external-auth/$(auth_id)/callback" python3 - >"$manifest_file" <<'PY'
import json, os
print(json.dumps({
    "name": os.environ["NAME"][:34],
    "url": "https://openflows.dev",
    "redirect_url": f"http://localhost:{os.environ['PORT']}/created",
    "callback_urls": [os.environ["CB"]],
    "public": True,
    "request_oauth_on_install": False,
    "default_permissions": {
        "metadata": "read", "contents": "write",
        "pull_requests": "write", "workflows": "write",
    },
}))
PY
    # Tiny local server: serves an auto-submitting form to GitHub, then
    # captures the one-time `code` GitHub redirects back with.
    PORT="$port" STATE="$state" TARGET="$target" MANIFEST_FILE="$manifest_file" OUT="$out_file" python3 - <<'PY' &
import html, http.server, os, urllib.parse
manifest = open(os.environ["MANIFEST_FILE"]).read()
state, target, out = os.environ["STATE"], os.environ["TARGET"], os.environ["OUT"]
class H(http.server.BaseHTTPRequestHandler):
    def log_message(self, *a): pass
    def _send(self, body):
        self.send_response(200); self.send_header("Content-Type", "text/html"); self.end_headers()
        self.wfile.write(body.encode())
    def do_GET(self):
        u = urllib.parse.urlparse(self.path); q = urllib.parse.parse_qs(u.query)
        if u.path == "/created" and q.get("state", [""])[0] == state and "code" in q:
            open(out, "w").write(q["code"][0])
            self._send("<h2>GitHub App created.</h2><p>Return to your terminal.</p>")
            self.server.done = True
        elif u.path == "/":
            self._send(f'<form id=f method=post action="{html.escape(target)}?state={state}">'
                       f'<input type=hidden name=manifest value="{html.escape(manifest)}"></form>'
                       '<p>Redirecting to GitHub…</p><script>document.getElementById("f").submit()</script>')
        else:
            self.send_response(404); self.end_headers()
s = http.server.HTTPServer(("127.0.0.1", int(os.environ["PORT"])), H); s.done = False
s.timeout = 1
import time; end = time.time() + 600
while not s.done and time.time() < end: s.handle_request()
PY
    local server_pid=$!
    info "Opening GitHub to create the app — click 'Create GitHub App' there."
    info "If no browser opens, visit: http://localhost:${port}/"
    open_url "http://localhost:${port}/"
    local i
    for ((i = 0; i < 600; i++)); do
        [ -s "$out_file" ] && break
        kill -0 "$server_pid" 2>/dev/null || break
        sleep 1
    done
    kill "$server_pid" 2>/dev/null || true
    code="$(cat "$out_file" 2>/dev/null || true)"
    rm -f "$out_file" "$manifest_file"
    [ -n "$code" ] || return 1
    local resp cid secret slug
    resp="$(curl -fsS -m 30 -X POST -H 'Accept: application/vnd.github+json' "https://api.github.com/app-manifests/${code}/conversions")" || return 1
    cid="$(printf '%s' "$resp" | json client_id)"
    secret="$(printf '%s' "$resp" | json client_secret)"
    slug="$(printf '%s' "$resp" | json slug)"
    [ -n "$cid" ] && [ -n "$secret" ] && [ -n "$slug" ] || return 1
    env_set CODER_EXTERNAL_AUTH_0_CLIENT_ID "$cid"
    env_set CODER_EXTERNAL_AUTH_0_CLIENT_SECRET "$secret"
    env_set CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL "https://github.com/apps/${slug}/installations/new"
    return 0
}

github_app_manual() {
    local owner="$1" new_url="https://github.com/settings/apps/new"
    [ -n "$owner" ] && new_url="https://github.com/settings/apps/new  (orgs: https://github.com/organizations/${owner}/settings/apps/new)"
    info "Create the app by hand:"
    info "  1. Open  ${new_url}"
    info "  2. Callback URL:  $(public_url)/external-auth/$(auth_id)/callback"
    info "  3. Permissions → Repository: Contents, Pull requests, Workflows = Read and write"
    info "  4. Create the app, generate a Client secret, then paste below."
    is_tty || die "GitHub App credentials are missing and no terminal is attached." \
        "Create the app (steps above), then set in .env:" \
        "  CODER_EXTERNAL_AUTH_0_CLIENT_ID, CODER_EXTERNAL_AUTH_0_CLIENT_SECRET, CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL"
    local cid secret slug
    cid="$(ask 'Client ID')"
    secret="$(ask_secret 'Client secret')"
    slug="$(ask 'App slug (the name in https://github.com/apps/<slug>)')"
    [ -n "$cid" ] && [ -n "$secret" ] && [ -n "$slug" ] || die "Client ID, secret and slug are all required."
    env_set CODER_EXTERNAL_AUTH_0_CLIENT_ID "$cid"
    env_set CODER_EXTERNAL_AUTH_0_CLIENT_SECRET "$secret"
    env_set CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL "https://github.com/apps/${slug}/installations/new"
}

stage_github_app() {
    step "GitHub App (lets agents work on your repo)"
    if ! is_placeholder "$(env_get CODER_EXTERNAL_AUTH_0_CLIENT_ID)" && ! is_placeholder "$(env_get CODER_EXTERNAL_AUTH_0_CLIENT_SECRET)"; then
        ok "already configured"
        return
    fi
    local owner="${REPO%%/*}"
    if is_tty && have python3 && github_app_via_manifest "$owner"; then
        ok "GitHub App created and saved to .env"
        local install_url
        install_url="$(env_get CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL)"
        panel "$C" "Install the app" "Choose 'Only select repositories' → ${REPO}" "${install_url}"
        open_url "$install_url"
        pause "Install the app on your repo, then continue"
    else
        warn "Automatic app creation unavailable — falling back to manual steps"
        github_app_manual "$owner"
        ok "GitHub App saved to .env"
        info "Install it on your repo: $(env_get CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL)"
        is_tty && pause "Install the app, then continue"
    fi
}

# ── stage 4: Docker stack ───────────────────────────────────────────────────
stage_docker() {
    step "Starting Redis + Coder"
    local out i
    if ! out="$(cd "$ROOT" && docker compose up -d 2>&1)"; then
        printf '%s\n' "$out" | tail -8 | sed 's/^/    /' >&2
        die "docker compose up failed (see above)."
    fi
    ( for ((i = 0; i < 90; i++)); do curl -fs -m 3 "$(coder_url)/api/v2/buildinfo" >/dev/null 2>&1 && exit 0; sleep 2; done; exit 1 ) &
    spin $! "Waiting for Coder to become healthy" \
        || die "Coder did not become healthy in 3 minutes." "Check logs: docker compose logs coder"
    ok "Coder is up at ${B}$(coder_url)${N}"
}

# ── stage 5: Coder admin + token ────────────────────────────────────────────
# mint_token — create a 90-day API token for the current CODER_TOKEN user (falls back to Coder's default lifetime).
# The token is copied into each tenant's nexus workspace, so it must outlive Coder's 7-day default.
mint_token() {
    local t name body
    name="openflows-setup-$(date +%Y%m%d%H%M%S)"   # unique: Coder rejects duplicate token names
    body="{\"token_name\":\"${name}\",\"lifetime\":7776000000000000}"
    t="$(api POST /api/v2/users/me/keys/tokens "$body" 2>/dev/null | json key || true)"
    [ -n "$t" ] || t="$(api POST /api/v2/users/me/keys/tokens "{\"token_name\":\"${name}\"}" | json key)"
    printf '%s' "$t"
}

# refresh_admin_token — expired token + saved password admin: log in again and mint a new token (works headless)
refresh_admin_token() {
    local email password session
    email="$(env_get CODER_ADMIN_EMAIL)"; password="$(env_get CODER_ADMIN_PASSWORD)"
    [ -n "$email" ] && [ -n "$password" ] || return 1
    session="$(api POST /api/v2/users/login "$(python3 -c 'import json,sys;print(json.dumps({"email":sys.argv[1],"password":sys.argv[2]}))' "$email" "$password")" 2>/dev/null | json session_token || true)"
    [ -n "$session" ] || return 1
    CODER_TOKEN="$session"
    local token; token="$(mint_token)"
    [ -n "$token" ] || return 1
    CODER_TOKEN="$token"
    env_set CODER_SESSION_TOKEN "$token"
}

# Headless fallback (no browser): create a password admin + token through the API.
create_admin_account() {
    local email username password session token
    email="$(env_get CODER_ADMIN_EMAIL)";       email="${email:-admin@openflows.dev}"
    username="$(env_get CODER_ADMIN_USERNAME)"; username="${username:-admin}"
    password="$(env_get CODER_ADMIN_PASSWORD)"
    if [ -z "$password" ]; then
        password="Of-$(rand_hex 8)-A1!"
        env_set CODER_ADMIN_PASSWORD "$password"   # persist first: a later failure must not lose it
    fi
    api POST /api/v2/users/first "$(python3 -c 'import json,sys;print(json.dumps({"email":sys.argv[1],"username":sys.argv[2],"password":sys.argv[3],"trial":False}))' "$email" "$username" "$password")" >/dev/null \
        || die "Could not create the Coder admin user."
    session="$(api POST /api/v2/users/login "$(python3 -c 'import json,sys;print(json.dumps({"email":sys.argv[1],"password":sys.argv[2]}))' "$email" "$password")" | json session_token)"
    [ -n "$session" ] || die "Could not log in to Coder as the new admin."
    CODER_TOKEN="$session"
    token="$(mint_token)"
    [ -n "$token" ] || die "Could not create a Coder API token."
    CODER_TOKEN="$token"
    env_set CODER_ADMIN_EMAIL "$email"
    env_set CODER_ADMIN_USERNAME "$username"
    env_set CODER_SESSION_TOKEN "$token"
    ok "admin '${username}' created, token saved to .env"
    info "Dashboard login: ${email} / see CODER_ADMIN_PASSWORD in .env"
}

stage_coder_token() {
    step "Your Coder account"
    local me first_status
    CODER_TOKEN="$(env_get CODER_SESSION_TOKEN)"
    if [ -n "$CODER_TOKEN" ] && me="$(api GET /api/v2/users/me 2>/dev/null)"; then
        ok "signed in as ${B}$(printf '%s' "$me" | json username)${N}"
        return
    fi
    CODER_TOKEN=""
    if refresh_admin_token; then
        ok "token expired — refreshed it with the saved admin login"
        return
    fi
    first_status="$(curl -s -m 10 -o /dev/null -w '%{http_code}' "$(coder_url)/api/v2/users/first")"
    if [ "$first_status" != "200" ]; then   # Coder has no users yet
        if ! is_tty || [ "${OPENFLOWS_SETUP_ADMIN:-}" = 1 ]; then
            create_admin_account
            return
        fi
        if [ "$(curl -s -m 10 "$(coder_url)/api/v2/users/authmethods" | json github.enabled || true)" != "True" ]; then
            warn "This Coder has no GitHub sign-in enabled — using a password admin instead"
            create_admin_account
            return
        fi
        # The first GitHub sign-in becomes the Coder owner — so it is YOUR account that owns the workspaces.
        info "Sign in to Coder with your GitHub account (you become the owner):"
        info "  1. Open   $(coder_url)"
        info "  2. Click 'Continue with GitHub' and authorize"
        open_url "$(coder_url)"
        ( for ((i = 0; i < 450; i++)); do
            [ "$(curl -s -m 5 -o /dev/null -w '%{http_code}' "$(coder_url)/api/v2/users/first")" = "200" ] && exit 0
            sleep 2
          done; exit 1 ) &
        spin $! "Waiting for you to sign in (up to 15 min)" \
            || die "Nobody signed in to Coder in time." "Open $(coder_url) and click 'Continue with GitHub'."
        ok "signed in"
    fi
    # A user exists: we need an API token for it. Only the signed-in user can create one.
    is_tty || die "Coder has users but .env has no working CODER_SESSION_TOKEN." \
        "Create a token at $(coder_url)/settings/tokens and put it in .env as CODER_SESSION_TOKEN."
    info "Now create an API token so the setup can act as you:"
    info "  $(public_url)/settings/tokens   → Create Token → copy it"
    info "  Pick the longest lifetime offered: the controller in each tenant workspace uses this token."
    open_url "$(coder_url)/settings/tokens"
    CODER_TOKEN="$(ask_secret 'Paste the token')"
    me="$(api GET /api/v2/users/me 2>/dev/null)" || die "That token was rejected by Coder." "Create a new one at $(coder_url)/settings/tokens"
    env_set CODER_SESSION_TOKEN "$CODER_TOKEN"
    ok "token saved — acting as ${B}$(printf '%s' "$me" | json username)${N}"
}

# parse_models PROVIDER — read a provider's model-list JSON on stdin, print "id<TAB>context" lines
parse_models() {
    python3 -c '
import sys, json
p = sys.argv[1]; d = json.load(sys.stdin); rows = []
if p == "openrouter":
    rows = [(m["id"], m.get("context_length") or 0) for m in d["data"]]
elif p == "openai":
    skip = ("embedding","whisper","tts","dall-e","moderation","davinci","babbage","audio","realtime","transcribe","image","search")
    ids = sorted((m["id"] for m in d["data"] if m["id"].startswith(("gpt","o","chatgpt")) and not any(x in m["id"] for x in skip)), reverse=True)
    rows = [(i, 0) for i in ids]
elif p == "anthropic":
    rows = [(m["id"], m.get("max_input_tokens") or 0) for m in d["data"]]
elif p == "google":
    rows = [(m["name"].split("/", 1)[-1], m.get("inputTokenLimit") or 0) for m in d["models"] if "generateContent" in m.get("supportedGenerationMethods", [])]
for i, c in rows: print(f"{i}\t{c}")
' "$1"
}

# fetch_models PROVIDER KEY FILE — write "id<TAB>context" lines; fails on a bad key or no network
fetch_models() {
    local provider="$1" key="$2" out="$3" url resp
    local hdr=()
    case "$provider" in
        openrouter)
            curl -fsS -m 20 -H "Authorization: Bearer ${key}" "https://openrouter.ai/api/v1/auth/key" >/dev/null 2>&1 || return 1   # /models is public, so check the key itself
            url="https://openrouter.ai/api/v1/models" ;;
        openai)     url="https://api.openai.com/v1/models"; hdr=(-H "Authorization: Bearer ${key}") ;;
        anthropic)  url="https://api.anthropic.com/v1/models?limit=1000"; hdr=(-H "x-api-key: ${key}" -H "anthropic-version: 2023-06-01") ;;
        google)     url="https://generativelanguage.googleapis.com/v1beta/models?pageSize=1000"; hdr=(-H "x-goog-api-key: ${key}") ;;
        *) return 1 ;;
    esac
    resp="$(curl -fsS -m 20 ${hdr[@]+"${hdr[@]}"} "$url" 2>/dev/null)" || return 1
    printf '%s' "$resp" | parse_models "$provider" >"$out" 2>/dev/null
    [ -s "$out" ]
}

# choose_model FILE DEFAULT — arrow-key picker over the provider's models + a search option; prints the id on stdout
choose_model() {
    local file="$1" def="$2" query="" i id total idx
    local shown=()
    total="$(wc -l <"$file" | tr -d ' ')"
    while true; do
        shown=()
        while IFS= read -r id; do [ -n "$id" ] && shown+=("$id"); done < <(python3 -c '
import sys
q = sys.argv[2].lower().split()
ids = [l.split("\t")[0] for l in open(sys.argv[1]).read().splitlines()]
hits = [i for i in ids if all(w in i.lower() for w in q)]
if not q:
    hits.sort(key=lambda i: not i.startswith(("anthropic/", "openai/", "google/")))
    if sys.argv[3] in hits: hits.remove(sys.argv[3]); hits.insert(0, sys.argv[3])
print("\n".join(hits[:10]))' "$file" "$query" "$def")
        if [ "${#shown[@]}" -eq 0 ]; then
            printf '%s%s%s  no model matches "%s"\n' "$(rail)" "$Y" "$N" "$query" >&2; query=""; continue
        fi
        idx="$(menu "Pick a model · ${total} available${query:+ · matching \"$query\"}" "${shown[@]}" "🔍 Search by name…")"
        if [ "$idx" -lt "${#shown[@]}" ]; then printf '%s' "${shown[idx]}"; return; fi
        query="$(ask 'Search for')"
    done
}

# ── stage 6: LLM ────────────────────────────────────────────────────────────
stage_llm() {
    step "LLM model for the agents"
    local org models
    org="$(api GET /api/v2/organizations | json 0.id)"
    [ -n "$org" ] || die "Could not read the Coder organization."
    models="$(api GET "/api/v2/organizations/${org}/chats/models" | python3 -c 'import sys,json;d=json.load(sys.stdin);print(len(d.get("models") or [m for p in d.get("providers",[]) for m in p.get("models",[])]))' 2>/dev/null || echo 0)"
    if [ "${models:-0}" -gt 0 ]; then
        ok "${models} model(s) already configured"
        return
    fi
    local choice="${OPENFLOWS_LLM_PROVIDER:-}" key="${OPENFLOWS_LLM_API_KEY:-}" model="${OPENFLOWS_LLM_MODEL:-}" ptype base def_model ctx
    if [ -z "$choice" ] || [ -z "$key" ]; then
        is_tty || die "No LLM configured and no terminal attached." \
            "Set OPENFLOWS_LLM_PROVIDER (anthropic|openai|openrouter|google), OPENFLOWS_LLM_API_KEY" \
            "(optionally OPENFLOWS_LLM_MODEL), or add one at $(coder_url)/ai/settings/providers"
        case "$(menu 'Which LLM provider?' 'Anthropic   · Claude models' 'OpenAI      · GPT models' 'OpenRouter  · hundreds of models, one key' 'Google      · Gemini models')" in
            1) choice=openai ;; 2) choice=openrouter ;; 3) choice=google ;; *) choice=anthropic ;;
        esac
        key="$(ask_secret "${choice} API key")"
    fi
    case "$choice" in
        anthropic)  ptype=anthropic;  base="https://api.anthropic.com";                       def_model="claude-sonnet-4-5"; ctx=200000 ;;
        openai)     ptype=openai;     base="https://api.openai.com/v1";                       def_model="gpt-4.1"; ctx=128000 ;;
        openrouter) ptype=openrouter; base="https://openrouter.ai/api/v1";                    def_model="anthropic/claude-sonnet-4.5"; ctx=200000 ;;
        google)     ptype=google;     base="https://generativelanguage.googleapis.com";       def_model="gemini-2.5-pro"; ctx=1000000 ;;
        *) die "Unknown provider '$choice' (use anthropic, openai, openrouter or google)." ;;
    esac
    [ -n "$key" ] || die "An API key is required."
    local list_file have_list=0 attempt new_key
    list_file="$(mktemp)"
    for attempt in 1 2 3; do
        if fetch_models "$ptype" "$key" "$list_file"; then have_list=1; break; fi
        is_tty && [ -z "$model" ] || break
        warn "Could not list models — check the API key (or your connection)."
        [ "$attempt" = 3 ] && break
        new_key="$(ask_secret 'Re-enter the API key, or press Enter to type a model name by hand')"
        [ -z "$new_key" ] && break
        key="$new_key"
    done
    if [ "$have_list" = 1 ]; then ok "API key accepted"; fi
    if [ -z "$model" ]; then
        if ! is_tty; then model="$def_model"
        elif [ "$have_list" = 1 ]; then model="$(choose_model "$list_file" "$def_model")"
        else model="$(ask 'Model' "$def_model")"
        fi
    fi
    if [ "$have_list" = 1 ]; then
        local listed_ctx
        listed_ctx="$(awk -F'\t' -v m="$model" '$1==m{print $2; exit}' "$list_file")"
        [ "${listed_ctx:-0}" -gt 0 ] && ctx="$listed_ctx"
    fi
    rm -f "$list_file"
    local provider_id
    provider_id="$(api POST /api/v2/ai/providers "$(python3 -c 'import json,sys;t,u,k=sys.argv[1:4];print(json.dumps({"type":t,"name":t,"base_url":u,"api_keys":[k],"enabled":True}))' "$ptype" "$base" "$key")" | json id)" \
        || die "Coder rejected the provider." "Add it by hand at $(coder_url)/ai/settings/providers"
    [ -n "$provider_id" ] || die "Coder returned no provider id."
    api POST "/api/v2/organizations/${org}/chats/models" "$(python3 -c 'import json,sys;p,m,c=sys.argv[1:4];print(json.dumps({"ai_provider_id":p,"model":m,"display_name":m,"enabled":True,"is_default":True,"context_limit":int(c)}))' "$provider_id" "$model" "$ctx")" >/dev/null \
        || die "Coder rejected the model '${model}'." "Add it by hand at $(coder_url)/ai/settings/models"
    ok "${ptype} / ${model} configured as the default"
}

# ── stage 7: bootstrap (templates) ──────────────────────────────────────────
# templates_ready — true when all five openflows-* templates exist and the dev binary is newer than every source file
templates_ready() {
    local bin="${ROOT}/.dev-binaries/openflows"
    [ -x "$bin" ] || return 1
    # Rebuild when any source or template is newer than the built binary.
    if [ -n "$(find "${ROOT}/crates" "${ROOT}/binary" "${ROOT}/Cargo.lock" -type f -newer "$bin" -print -quit 2>/dev/null)" ]; then
        return 1
    fi
    api GET /api/v2/templates 2>/dev/null | python3 -c '
import sys, json
names = {t.get("name") for t in json.load(sys.stdin)}
sys.exit(0 if {"openflows-"+r for r in ("forge","sentinel","nexus","vessel","lore")} <= names else 1)' 2>/dev/null
}

stage_bootstrap() {
    step "Workspace templates (first run takes ~10 min)"
    CODER_TOKEN="$(env_get CODER_SESSION_TOKEN)"
    if [ "${OPENFLOWS_SETUP_FORCE_BOOTSTRAP:-}" != 1 ] && templates_ready; then
        ok "binaries and 5 templates already in place"
        info "Sources unchanged since the last build. Force a rebuild: OPENFLOWS_SETUP_FORCE_BOOTSTRAP=1 ./scripts/setup.sh"
        return
    fi
    info "Safe to wait — details are logged to .setup.log"
    run_quiet "Building binaries and pushing templates" "${SCRIPT_DIR}/prod.sh" bootstrap
    ok "binaries built, 5 templates pushed"
}

# ensure_github_link — Coder only builds workspaces once the OWNING Coder account (the token's user) has linked the GitHub App.
ensure_github_link() {
    local linked me user login url
    CODER_TOKEN="$(env_get CODER_SESSION_TOKEN)"
    linked="$(api GET "/api/v2/external-auth/$(auth_id)" 2>/dev/null | json authenticated || true)"
    if [ "$linked" = "True" ]; then ok "GitHub already linked"; return; fi
    me="$(api GET /api/v2/users/me 2>/dev/null || true)"
    user="$(printf '%s' "$me" | json username)"; login="$(printf '%s' "$me" | json login_type)"
    url="$(public_url)/external-auth/$(auth_id)"
    if [ "$login" = "password" ]; then
        panel "$Y" "Link GitHub (one time)" \
            "This token belongs to the password account '${user}'." \
            "Open a private window, then:" \
            "  1. Open  ${url}" \
            "  2. Sign in as  $(env_get CODER_ADMIN_EMAIL)  (password: CODER_ADMIN_PASSWORD in .env)" \
            "  3. Click the GitHub button and Authorize"
    else
        panel "$C" "Link GitHub (one time)" \
            "Agents act on your repo through your GitHub account." \
            "  1. Open  ${url}   (signed in as '${user}')" \
            "  2. Click the GitHub button and Authorize" \
            "  3. Install the app on ${REPO} if asked"
        open_url "$url"
    fi
    if ! is_tty; then   # no terminal to wait in: tenant add offers a device-flow code and waits itself
        warn "No terminal — continuing; the tenant step will ask you to authorize GitHub"
        return
    fi
    ( for ((i = 0; i < 300; i++)); do
        [ "$(api GET "/api/v2/external-auth/$(auth_id)" 2>/dev/null | json authenticated || true)" = "True" ] && exit 0
        sleep 2
      done; exit 1 ) &
    spin $! "Waiting for you to link GitHub (up to 10 min)" \
        || die "GitHub was not linked in time." "Make sure the browser is signed in to Coder as '${user}' at ${url}."
    ok "GitHub linked"
}

# check_existing_tenant — `tenant add` returns an existing workspace unchanged, so a tenant name already bound
# to a different repo would keep running against the old one. Stop with the fix instead of reporting success.
check_existing_tenant() {
    local ws build existing
    CODER_TOKEN="$(env_get CODER_SESSION_TOKEN)"
    ws="$(api GET "/api/v2/users/me/workspace/openflows-nexus-${NAME}" 2>/dev/null || true)"
    build="$(printf '%s' "$ws" | json latest_build.id)"
    [ -n "$build" ] || return 0   # no workspace yet: nothing to reuse
    existing="$(api GET "/api/v2/workspacebuilds/${build}/parameters" 2>/dev/null | python3 -c '
import sys, json
for p in json.load(sys.stdin):
    if p.get("name") == "github_repository": print(p.get("value", ""))' 2>/dev/null || true)"
    if [ -n "$existing" ] && [ "$existing" != "$REPO" ]; then
        die "Tenant '${NAME}' already exists for ${existing}" \
            "An existing tenant workspace is reused unchanged, so it would keep working on ${existing}." \
            "Either pick another name:  ./scripts/setup.sh ${REPO} --name <other-name>" \
            "or delete the old workspace and re-run:  coder delete openflows-nexus-${NAME} --yes"
    fi
}

# ── stage 8: tenant + doctor ────────────────────────────────────────────────
stage_tenant() {
    step "Connecting ${REPO}"
    check_existing_tenant
    ensure_github_link
    printf '\n'
    "${SCRIPT_DIR}/prod.sh" tenant "$REPO" --name "$NAME" --fleet "$FLEET"
    printf '\n'
    ok "tenant '${NAME}' created"
    if "${SCRIPT_DIR}/prod.sh" doctor >"$LOG_FILE" 2>&1; then
        ok "health check passed"
    else
        sed 's/\x1b\[[0-9;]*m//g; s/^/    /' "$LOG_FILE" | tail -n 15 >&2
        die "Health check failed" "The tenant was created, but ./scripts/prod.sh doctor reports problems." "Full output: ${LOG_FILE}"
    fi
}

# ── main ────────────────────────────────────────────────────────────────────
main() {
    REPO=""; NAME=""; FLEET=""
    while [ $# -gt 0 ]; do
        case "$1" in
            --name) NAME="${2:?--name needs a value}"; shift 2 ;;
            --fleet) FLEET="${2:?--fleet needs a value}"; shift 2 ;;
            -h|--help) sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
            -*) die "Unknown option: $1" ;;
            *) REPO="$1"; shift ;;
        esac
    done
    cd "$ROOT"
    banner
    is_tty && trap 'printf "\033[?25h"' EXIT

    # Resume: reuse the repo/name/fleet from the previous run (pass owner/repo to change it).
    local saved_repo; saved_repo="$(env_get OPENFLOWS_SETUP_REPO)"
    if [ -z "$REPO" ] && [ -n "$saved_repo" ]; then
        REPO="$saved_repo"
        [ -n "$NAME" ] || NAME="$(env_get OPENFLOWS_SETUP_NAME)"
        [ -n "$FLEET" ] || FLEET="$(env_get OPENFLOWS_SETUP_FLEET)"
        printf '  %sResuming setup for %s%s%s — finished steps are skipped. (Different repo: ./scripts/setup.sh owner/repo)%s\n' "$D" "$N$B" "$REPO" "$N$D" "$N"
    fi
    if [ -z "$REPO" ]; then
        local guess
        guess="$(git -C "$ROOT" remote get-url origin 2>/dev/null | sed -E 's#(git@github.com:|https://github.com/)##; s#\.git$##' || true)"
        is_tty || die "Pass the repo to connect: ./scripts/setup.sh owner/repo"
        REPO="$(ask 'GitHub repo to connect (owner/repo)' "$guess")"
    fi
    [[ "$REPO" =~ ^[A-Za-z0-9_.-]+/[A-Za-z0-9_.-]+$ ]] || die "Repo must look like owner/repo (got '${REPO}')."
    if [ "$REPO" = "$saved_repo" ]; then   # same repo as last time: keep its tenant name/fleet so the workspace is reused, not duplicated
        [ -n "$NAME" ] || NAME="$(env_get OPENFLOWS_SETUP_NAME)"
        [ -n "$FLEET" ] || FLEET="$(env_get OPENFLOWS_SETUP_FLEET)"
    fi
    NAME="${NAME:-${REPO/\//-}}"   # new repo: owner-repo, so orgA/backend and orgB/backend do not collide
    NAME="$(printf '%s' "$NAME" | tr -c 'A-Za-z0-9._-' '-' | sed 's/-*$//')"
    if [ -z "$FLEET" ] && is_tty; then   # first run, no --fleet given: ask (a re-run reuses the saved value)
        printf '\n  %sFleet = FORGE-SENTINEL pairs (a builder plus its reviewer). 1 pair works one issue at a time;\n  N pairs work N issues in parallel, using more LLM tokens and workspaces. Start small — you can add more later.%s\n' "$D" "$N"
        while true; do
            FLEET="$(ask 'How many FORGE-SENTINEL pairs?' 1)"
            [[ "$FLEET" =~ ^[1-9][0-9]*$ ]] && break
            warn "Enter a whole number, 1 or more"
        done
    fi
    FLEET="${FLEET:-1}"
    [[ "$FLEET" =~ ^[1-9][0-9]*$ ]] || die "--fleet must be a whole number >= 1."

    stage_preflight
    stage_env
    env_set OPENFLOWS_SETUP_REPO "$REPO"; env_set OPENFLOWS_SETUP_NAME "$NAME"; env_set OPENFLOWS_SETUP_FLEET "$FLEET"
    stage_github_app
    stage_docker
    stage_coder_token
    stage_llm
    stage_bootstrap
    stage_tenant

    close_step
    printf '\n'
    RAIL_OPEN=0
    panel "$G" "✓ All set · $((SECONDS - START_TIME))s" "" \
        "Open an issue in ${REPO} and OpenFlows picks it up." "" \
        "Fleet        ${FLEET} FORGE-SENTINEL pair(s)" \
        "Dashboard    $(coder_url)" \
        "Health       ./scripts/prod.sh doctor" \
        "Add a repo   ./scripts/prod.sh tenant owner/repo --name team --fleet 1" ""
    printf '\n'
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then main "$@"; fi
