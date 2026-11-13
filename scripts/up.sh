#!/usr/bin/env bash
# One command to start the whole local control plane: Coder, the databases,
# Redis, the runtime relay, the Manager and the Console.
#
#   ./scripts/up.sh           start (or update) everything
#   ./scripts/up.sh --pull    also fetch a newer Console image first
#
# This wraps `docker compose -f deployment.compose.yml up -d` and does the host
# preparation that command cannot do on its own: it detects the user and
# Docker group ids the Manager container must run as, creates the provisioner
# credential directory (so Docker does not create it as root), and checks that
# .env.openflows points Coder and the workspaces at the Compose Manager. It
# then waits until the Manager and the Console answer and prints their URLs.
#
# The Manager runs in Compose (http://manager:3002), so no host Manager, host
# port or firewall rule is involved. An install first set up with a Manager run
# on the host (`cargo run ... serve`) saved CODER_URL=http://localhost:7080;
# this script stops with the exact fix instead of editing saved state.
#
# Prerequisites it does not create: .env.openflows (copy .env.openflows.example
# and fill it in) and the secret files described in the operator runbook.

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
ENV_FILE="${ROOT}/.env.openflows"
COMPOSE=(docker compose --env-file "$ENV_FILE" -f "${ROOT}/deployment.compose.yml")

die() { printf 'error: %s\n' "$1" >&2; [ -z "${2:-}" ] || printf '%s\n' "$2" >&2; exit 1; }
# The value Compose would read from the env file: the last assignment, without surrounding quotes.
env_get() {
    grep -E "^$1=" "$ENV_FILE" 2>/dev/null | tail -n1 | cut -d= -f2- \
        | sed -E "s/^\"(.*)\"\$/\\1/; s/^'(.*)'\$/\\1/" || true
}
env_set() { # env_set KEY VALUE: replace the line or append
    if grep -qE "^$1=" "$ENV_FILE"; then sed -i "s#^$1=.*#$1=$2#" "$ENV_FILE"; else printf '%s=%s\n' "$1" "$2" >>"$ENV_FILE"; fi
}

PULL=0
for arg in "$@"; do
    case "$arg" in
        --pull) PULL=1 ;;
        -h|--help) sed -n '2,10p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "Unknown option: $arg" ;;
    esac
done

command -v docker >/dev/null 2>&1 || die "docker is not installed."
docker compose version >/dev/null 2>&1 || die "docker compose v2 is not installed."
[ -f "$ENV_FILE" ] || die ".env.openflows is missing." \
    "Copy .env.openflows.example to .env.openflows, fill it in and create the secrets (operator-local-runbook.md, steps 1-2)."

# The compose file mounts these two secret files individually; Docker fails with an unhelpful "bind
# source path does not exist" if they are missing. The other secrets (GitHub App key and so on) are
# read by the Manager from the mounted directory under names the *_REF settings can change, so they
# are not checked here. An install made with ./scripts/setup.sh keeps its secrets elsewhere and runs a
# different Manager (openflows-local-manager), so it must not use this script.
SECRET_DIR="${OPENFLOWS_SECRET_DIR:-$(env_get OPENFLOWS_SECRET_DIR)}"; SECRET_DIR="${SECRET_DIR:-.openflows/secrets}"
case "$SECRET_DIR" in /*) ;; *) SECRET_DIR="${ROOT}/${SECRET_DIR}" ;; esac
missing=()
for f in coder-hook-secret coder-github-client-secret; do
    [ -f "${SECRET_DIR}/${f}" ] || missing+=("$f")
done
if [ "${#missing[@]}" -gt 0 ]; then
    die "Missing secret file(s) in ${SECRET_DIR}: ${missing[*]}" \
"This script is for the operator setup (operator-local-runbook.md, steps 1-2), which creates these files.
If you installed with ./scripts/setup.sh, your stack is different: do not run this script."
fi

# Checked before anything is changed, so a host-Manager install is left untouched.
# An install set up with a host Manager saved a different CODER_URL; the Manager refuses to start until they match.
if [ -f "${SECRET_DIR}/deployment.json" ]; then
    saved="$(python3 -c 'import json,sys; print(json.load(open(sys.argv[1])).get("coder_url",""))' "${SECRET_DIR}/deployment.json")"
    [ "$saved" = "http://coder:7080" ] || die "Setup was saved for a host Manager (coder_url=${saved})." \
"Either keep running the Manager on the host, or switch this install to Compose by changing
coder_url in ${SECRET_DIR}/deployment.json to http://coder:7080 (back the file up first)."
fi

# The Manager container runs as the user who owns the secret files and needs
# the Docker group to create tenant containers. Set these in .env.openflows to override.
[ -n "${OPENFLOWS_MANAGER_UID:-$(env_get OPENFLOWS_MANAGER_UID)}" ] || env_set OPENFLOWS_MANAGER_UID "$(id -u)"
[ -n "${OPENFLOWS_MANAGER_GID:-$(env_get OPENFLOWS_MANAGER_GID)}" ] || env_set OPENFLOWS_MANAGER_GID "$(id -g)"
if [ -z "${DOCKER_GID:-$(env_get DOCKER_GID)}" ]; then
    env_set DOCKER_GID "$(stat -c %g /var/run/docker.sock 2>/dev/null || stat -f %g /var/run/docker.sock 2>/dev/null || echo 999)"
fi

# Same absolute path on the host and in the container: the host Docker daemon bind-mounts it into tenants.
PROV_DIR="${OPENFLOWS_PROVISIONER_SECRET_DIR:-$(env_get OPENFLOWS_PROVISIONER_SECRET_DIR)}"
[ -n "$PROV_DIR" ] || { PROV_DIR="${ROOT}/.local/provisioners"; env_set OPENFLOWS_PROVISIONER_SECRET_DIR "$PROV_DIR"; }
mkdir -p "$PROV_DIR"

# Coder and the workspaces reach the Compose Manager by service name. Plain HTTP needs the insecure-hook flag.
if ! grep -q "^OPENFLOWS_HOOK_URL=http://manager:3002/" "$ENV_FILE" \
    || [ "$(env_get OPENFLOWS_RUNTIME_MANAGER_URL)" != "http://manager:3002" ]; then
    cp -p "$ENV_FILE" "${ENV_FILE}.bak"
    env_set OPENFLOWS_HOOK_URL "http://manager:3002/api/v1/hooks/coder/chat"
    env_set OPENFLOWS_RUNTIME_MANAGER_URL "http://manager:3002"
    env_set CODER_CHAT_HOOK_ALLOW_INSECURE "true"
    printf 'Pointed the Coder hook and runtime URLs at the Compose Manager (backup: .env.openflows.bak).\n'
fi

[ "$PULL" = 0 ] || "${COMPOSE[@]}" pull console
"${COMPOSE[@]}" up -d --build

printf 'Waiting for the Manager'
for _ in $(seq 1 90); do
    curl -fs -m 3 http://127.0.0.1:3002/ready >/dev/null 2>&1 && break
    printf '.'; sleep 2
done
printf '\n'
curl -fs -m 3 http://127.0.0.1:3002/ready >/dev/null 2>&1 \
    || die "The Manager did not become ready." "Check: ${COMPOSE[*]} logs manager"
curl -fs -m 5 -o /dev/null http://127.0.0.1:3000/login \
    || die "The Console did not answer." "Check: ${COMPOSE[*]} logs console"

cat <<EOF

Everything is running.
  Console   http://127.0.0.1:3000
  Manager   http://127.0.0.1:3002
  Coder     $(env_get CODER_URL)
EOF
