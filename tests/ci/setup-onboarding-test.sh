#!/usr/bin/env bash
# Tests what scripts/setup.sh shows before starting and how it checks your own GitHub App (#486),
# with stand-ins for Coder's API and the terminal.
#
# Usage: bash tests/ci/setup-onboarding-test.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=../../scripts/setup.sh
source "${ROOT}/scripts/setup.sh"   # main only runs when setup.sh is executed, not sourced
set +e

failures=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }
expect_has() {   # expect_has description output text
    case "$2" in *"$3"*) pass "$1" ;; *) fail "$1 (no '$3' in: $2)" ;; esac
}
expect_lacks() {   # expect_lacks description output text
    case "$2" in *"$3"*) fail "$1 (found '$3')" ;; *) pass "$1" ;; esac
}

WORK="$(mktemp -d)"
trap 'rm -r -- "$WORK"' EXIT
ENV_FILE="${WORK}/.env"
REPO=Acme/private-repo; NAME=acme; FLEET=2
unset OPENFLOWS_GITHUB_AUTH_MODE

echo "What setup shows before starting"
plan() {   # plan INSTALL_MODE APP_MODE
    MODE="$1"
    printf 'OPENFLOWS_GITHUB_AUTH_MODE=%s\nOPENFLOWS_VERSION=1.9.0\n' "$2" >"$ENV_FILE"
    show_install_plan 2>&1
}
out="$(plan binary connected-local)"
expect_has "the repository" "$out" "Acme/private-repo"
expect_has "the shared App" "$out" "shared OpenFlows App"
expect_has "the release" "$out" "release 1.9.0"
expect_has "the fleet" "$out" "2 FORGE-SENTINEL pair(s)"
out="$(plan compose self-managed)"
expect_has "your own App" "$out" "your own App"
expect_has "the image" "$out" "image 1.9.0"
expect_has "compiling" "$(plan source self-managed)" "source (compiled from this checkout)"
expect_lacks "never prints a secret" "$(printf 'CODER_EXTERNAL_AUTH_0_CLIENT_SECRET=hunter2\n' >>"$ENV_FILE"; MODE=source; show_install_plan 2>&1)" hunter2

echo "Your own App is installed on the repo's owner"
is_tty() { [ "${FAKE_TTY:-}" = yes ]; }
open_url() { :; }
INSTALLED='{"app_installable":true,"installations":[{"account":{"login":"acme"},"configure_url":"https://github.com/settings/installations/1"}]}'
OTHER='{"app_installable":true,"installations":[{"account":{"login":"someone"},"configure_url":"https://github.com/settings/installations/2"}]}'
UNKNOWN='{"app_installable":false,"installations":[]}'
api() { [ -n "${FAKE_API:-}" ] || return 1; cat "$FAKE_API"; }
check() {   # check RESPONSE [RESPONSE_AFTER_PAUSE] — prints the output, then `stopped` if setup stopped
    (
        FAKE_API="${WORK}/api.json"; printf '%s' "$1" >"$FAKE_API"
        AFTER="${2:-$1}"
        pause() { printf '%s' "$AFTER" >"$FAKE_API"; }
        set -e
        ensure_app_installed
        echo finished
    ) 2>&1 || echo stopped
}
out="$(check "$INSTALLED")"
expect_has "installed: continues" "$out" finished
expect_has "installed: says so, ignoring case" "$out" "installed on Acme"
expect_has "installed: links to its repository list" "$out" "https://github.com/settings/installations/1"
expect_has "installed: reminds about the repository" "$out" "Acme/private-repo"
out="$(check "$OTHER")"
expect_has "on another account, no terminal: stops" "$out" stopped
expect_has "on another account: says how to fix it" "$out" "CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL"
expect_lacks "on another account: does not continue" "$out" finished
expect_has "no installations: stops" "$(check '{"app_installable":true,"installations":[]}')" stopped
export FAKE_TTY=yes
out="$(check "$OTHER" "$INSTALLED")"
expect_has "installed while setup waits: continues" "$out" finished
out="$(check "$OTHER" "$OTHER")"
expect_has "still not installed after waiting: stops" "$out" stopped
unset FAKE_TTY
for response in "$UNKNOWN" "not json" ""; do
    out="$(check "$response")"
    expect_has "Coder can't tell ('${response:0:12}'): warns" "$out" "Could not check"
    expect_has "Coder can't tell ('${response:0:12}'): continues" "$out" finished
done

echo "When setup runs the check"
linked_calls() {   # linked_calls APP_MODE AUTHENTICATED — whether the App is checked
    (
        printf 'OPENFLOWS_GITHUB_AUTH_MODE=%s\n' "$1" >"$ENV_FILE"
        FAKE_AUTHENTICATED="$2"
        api() { printf '{"authenticated":%s}' "$FAKE_AUTHENTICATED"; }
        ensure_app_installed() { echo checked; }
        ensure_github_link 2>&1
    )
}
expect_has "already linked: checks" "$(linked_calls self-managed true)" checked
expect_lacks "not linked, no terminal: nothing to check yet" "$(linked_calls self-managed false)" checked
expect_lacks "the shared App: the console checks instead" "$(linked_calls connected-local true)" checked

echo "A new .env loads as shell code, as prod.sh does"
loads() { if bash -c 'set -euo pipefail; set -a; source "$1"' _ "$1" >/dev/null 2>&1; then echo yes; else echo no; fi; }
case "$(loads "${ROOT}/.env.example")" in yes) pass ".env.example" ;; *) fail ".env.example does not load" ;; esac
cleaned_env() {   # cleaned_env LINE — what stage_env leaves of an .env copied before this was fixed
    (
        ENV_FILE="${WORK}/old.env"; printf '%s\n' "$1" >"$ENV_FILE"
        stage_env >/dev/null 2>&1
        loads "$ENV_FILE"
        env_get CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL
    ) | paste -sd' ' -
}
case "$(cleaned_env 'CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL=https://github.com/apps/<your-app-slug>/installations/new')" in
    "yes ") pass "an old placeholder is cleared" ;; *) fail "an old placeholder stays" ;; esac
case "$(cleaned_env 'CODER_EXTERNAL_AUTH_0_APP_INSTALL_URL=https://github.com/apps/mine/installations/new')" in
    "yes https://github.com/apps/mine/installations/new") pass "a real install link stays" ;; *) fail "a real install link was changed" ;; esac

echo "The setup log"
log_mode() {
    (
        ROOT="${WORK}/log"; ENV_FILE="${ROOT}/.env"; LOG_FILE="${ROOT}/.setup.log"; mkdir -p "$ROOT"
        touch "$LOG_FILE"; chmod 644 "$LOG_FILE"
        main "not a repo" --binary >/dev/null 2>&1
    )
    stat -c '%a' "${WORK}/log/.setup.log" 2>/dev/null || stat -f '%Lp' "${WORK}/log/.setup.log"
}
case "$(log_mode)" in 600) pass "only you can read it" ;; *) fail "the log is readable by others ($(log_mode))" ;; esac

echo "The health check shows the same"
doctor_output() {   # doctor_output LINES_FOR_ENV
    (
        P="${WORK}/doctor-$RANDOM"; mkdir -p "${P}/scripts" "${P}/.dev-binaries"
        cp "${ROOT}/scripts/prod.sh" "${P}/scripts/"
        printf '%b' "$1" >"${P}/.env"
        printf '#!/bin/sh\necho doctor ran\n' >"${P}/.dev-binaries/openflows"
        chmod +x "${P}/.dev-binaries/openflows"
        env -u OPENFLOWS_BIN bash "${P}/scripts/prod.sh" doctor 2>&1
    )
}
out="$(doctor_output 'OPENFLOWS_SETUP_MODE=binary\nOPENFLOWS_SETUP_REPO=acme/repo\nOPENFLOWS_GITHUB_AUTH_MODE=connected-local\nOPENFLOWS_VERSION=1.9.0\nCODER_EXTERNAL_AUTH_0_CLIENT_SECRET=hunter2\n')"
expect_has "the repository" "$out" "acme/repo"
expect_has "the shared App" "$out" "shared OpenFlows App"
expect_has "the install" "$out" "release 1.9.0"
expect_has "and still runs the check" "$out" "doctor ran"
expect_lacks "never prints a secret" "$out" hunter2
out="$(doctor_output 'OPENFLOWS_SETUP_MODE=compose\nOPENFLOWS_GITHUB_AUTH_MODE=self-managed\nOPENFLOWS_VERSION=1.9.0\n')"
expect_has "your own App" "$out" "your own App"
expect_lacks "no repository yet: no empty line" "$out" "Repository"

echo "A new App gets the permissions the agents use"
app_manifest="$(sed -n '/"default_permissions"/,/}/p' "${ROOT}/scripts/setup.sh")"
for permission in '"metadata": "read"' '"actions": "read"' '"checks": "read"' '"statuses": "read"' \
    '"contents": "write"' '"issues": "write"' '"pull_requests": "write"' '"workflows": "write"'; do
    expect_has "$permission" "$app_manifest" "$permission"
done

echo "prod.sh talks to the Coder that setup started"
coder_url_seen() {   # coder_url_seen LINES_FOR_ENV
    (
        P="${WORK}/url-$RANDOM"; mkdir -p "${P}/scripts" "${P}/.dev-binaries"
        cp "${ROOT}/scripts/prod.sh" "${P}/scripts/"
        printf 'OPENFLOWS_SETUP_MODE=binary\n%b' "$1" >"${P}/.env"
        printf '#!/bin/sh\necho "url=$CODER_URL"\n' >"${P}/.dev-binaries/openflows"
        chmod +x "${P}/.dev-binaries/openflows"
        env -u OPENFLOWS_BIN -u CODER_URL -u CODER_PORT bash "${P}/scripts/prod.sh" doctor 2>&1 | sed -n 's/^url=//p'
    )
}
expect_has "the default port" "$(coder_url_seen '')" "http://localhost:7080"
expect_has "CODER_PORT" "$(coder_url_seen 'CODER_PORT=7186\n')" "http://localhost:7186"
expect_has "CODER_URL wins" "$(coder_url_seen 'CODER_PORT=7186\nCODER_URL=https://coder.test\n')" "https://coder.test"

echo
if [ "$failures" -gt 0 ]; then
    echo "$failures test(s) failed"
    exit 1
fi
echo "All onboarding tests passed"
