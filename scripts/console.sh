#!/usr/bin/env bash
# Start the OpenFlows Console and connect it to whatever Manager is already running,
# however OpenFlows was set up.
#
#   ./scripts/console.sh                  find the Manager, start the Console on :3000
#   ./scripts/console.sh --port 3100      serve the Console on another port
#   ./scripts/console.sh --manager-url U  use this Manager instead of searching for one
#   ./scripts/console.sh --no-pull        do not fetch a newer image first
#   ./scripts/console.sh --stop           remove the Console container
#
# The Console proxies sign-in and API calls to the Manager, so it must reach it from
# inside its container. Setups differ: ./scripts/setup.sh runs the Manager in Docker
# and publishes it on 127.0.0.1:3002; the operator Compose file does the same; a
# Manager run with `cargo run ... serve` may listen on 127.0.0.1 or on the Docker
# bridge address. Rather than assume one, this script looks for a ready Manager on the
# host's loopback and the Docker bridge address, then:
#   - Linux: runs the Console on the host network, so it reaches the Manager on the
#     address the script just verified. No bridge route, published port or firewall
#     rule is involved, and the Console listens on 127.0.0.1 only.
#   - Docker Desktop (macOS/Windows): publishes the port and reaches the Manager
#     through host.docker.internal.
# It then checks the whole path (Console -> Manager -> GitHub redirect) and reports
# whether GitHub will send the browser back to the Console. With no Manager running
# it offers to run ./scripts/setup.sh first (the GitHub App and LLM steps are
# interactive, so that part cannot be silent).
#
# The container is named openflows-console. Any Console container started by Compose
# (deployment.compose.yml) is stopped first, because only one can own the port; it is restarted if the
# new Console does not come up.
# Set CONSOLE_IMAGE_TAG to pin a version (default: develop).

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
NAME=openflows-console
IMAGE="ghcr.io/the-agenticflow/openflows-console:${CONSOLE_IMAGE_TAG:-develop}"
PORT=3000
MANAGER_URL=""
PULL=1
STOP=0

die() { printf 'error: %s\n' "$1" >&2; [ -z "${2:-}" ] || printf '%s\n' "$2" >&2; exit 1; }
note() { printf '%s\n' "$*"; }

while [ $# -gt 0 ]; do
    case "$1" in
        --port) PORT="${2:?--port needs a value}"; shift 2 ;;
        --manager-url) MANAGER_URL="${2:?--manager-url needs a value}"; shift 2 ;;
        --no-pull) PULL=0; shift ;;
        --stop) STOP=1; shift ;;
        -h|--help) sed -n '2,12p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; exit 0 ;;
        *) die "Unknown option: $1" ;;
    esac
done
[[ "$PORT" =~ ^[0-9]+$ ]] || die "--port must be a number."

command -v docker >/dev/null 2>&1 || die "docker is not installed."
docker info >/dev/null 2>&1 || die "Cannot talk to Docker." "Is the daemon running, and is your user in the docker group?"
command -v curl >/dev/null 2>&1 || die "curl is not installed."

if [ "$STOP" = 1 ]; then
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    note "Console stopped."
    exit 0
fi

# Docker Desktop has no real host network; everywhere else Linux host networking works.
DESKTOP=0
if [ "$(uname -s)" != Linux ] || docker info --format '{{.OperatingSystem}}' 2>/dev/null | grep -qi "docker desktop"; then
    DESKTOP=1
fi

manager_ready() { curl -fs -m 3 "$1/ready" >/dev/null 2>&1 || curl -fs -m 3 "$1/health" >/dev/null 2>&1; }

bridge_ip() {
    docker network inspect bridge --format '{{(index .IPAM.Config 0).Gateway}}' 2>/dev/null || true
}

# A URL the Manager answers on, as seen from this host.
find_manager() {
    local c bridge
    bridge="$(bridge_ip)"
    for c in http://127.0.0.1:3002 ${bridge:+"http://${bridge}:3002"} http://localhost:3002; do
        if manager_ready "$c"; then printf '%s' "$c"; return 0; fi
    done
    return 1
}

if [ -z "$MANAGER_URL" ]; then
    MANAGER_URL="$(find_manager || true)"
    if [ -z "$MANAGER_URL" ]; then
        note "No OpenFlows Manager answers on 127.0.0.1:3002 or the Docker bridge."
        if [ -t 0 ] && [ -t 1 ] && [ -x "${ROOT}/scripts/setup.sh" ]; then
            printf 'Run ./scripts/setup.sh now to install OpenFlows? [Y/n] '
            read -r answer
            case "${answer:-Y}" in
                [Nn]*) die "Start OpenFlows first, then re-run this script." ;;
            esac
            "${ROOT}/scripts/setup.sh"
            MANAGER_URL="$(find_manager || true)"
        fi
        [ -n "$MANAGER_URL" ] || die "No running OpenFlows Manager found." \
"Install and start OpenFlows first (./scripts/setup.sh owner/repo), then re-run this script.
If your Manager is elsewhere: ./scripts/console.sh --manager-url http://host:port"
    fi
fi
manager_ready "$MANAGER_URL" || die "The Manager at ${MANAGER_URL} does not answer /ready or /health."
note "Manager: ${MANAGER_URL}"

# Prepare the replacement image first, so a mistyped tag cannot leave you without a Console.
if [ "$PULL" = 1 ]; then
    docker pull -q "$IMAGE" >/dev/null 2>&1 \
        || docker image inspect "$IMAGE" >/dev/null 2>&1 \
        || die "Cannot pull ${IMAGE}." "Check your network, CONSOLE_IMAGE_TAG and that the image is public."
else
    docker image inspect "$IMAGE" >/dev/null 2>&1 || die "${IMAGE} is not available locally; re-run without --no-pull."
fi

# Only one Console can own the port. Stop ours and any started by Compose, but keep them
# so that they can be restarted if the new one does not come up.
OLD=()
ours="$(docker ps -aq --filter "name=^/${NAME}\$" 2>/dev/null || true)"
if [ -n "$ours" ]; then
    docker stop "$ours" >/dev/null 2>&1 || true
    docker rename "$ours" "${NAME}-old" >/dev/null
    OLD+=("$ours")
fi
for id in $(docker ps -q --filter label=com.docker.compose.service=console 2>/dev/null); do
    case "$(docker inspect --format '{{.Config.Image}}' "$id" 2>/dev/null)" in
        *openflows-console*)
            note "Stopping the Compose Console ($(docker inspect --format '{{.Name}}' "$id" | tr -d /))."
            docker stop "$id" >/dev/null; OLD+=("$id") ;;
    esac
done
rollback_and_die() { # rollback_and_die MESSAGE [DETAIL]: remove the new Console and restart the old ones
    docker rm -f "$NAME" >/dev/null 2>&1 || true
    for id in ${OLD[@]+"${OLD[@]}"}; do
        if [ "$(docker inspect --format '{{.Name}}' "$id" 2>/dev/null)" = "/${NAME}-old" ]; then
            docker rename "${NAME}-old" "$NAME" >/dev/null 2>&1 || true
        fi
        docker start "$id" >/dev/null 2>&1 || true
    done
    [ "${#OLD[@]}" -eq 0 ] || note "The previous Console was restarted."
    die "$@"
}

if (exec 3<>"/dev/tcp/127.0.0.1/${PORT}") 2>/dev/null; then
    rollback_and_die "Port ${PORT} is already in use." "Use another one: ./scripts/console.sh --port 3100"
fi

if [ "$DESKTOP" = 0 ]; then
    docker run -d --name "$NAME" --restart unless-stopped --network host \
        -e PORT="$PORT" -e HOSTNAME=127.0.0.1 \
        -e OPENFLOWS_MANAGER_URL="$MANAGER_URL" "$IMAGE" >/dev/null \
        || rollback_and_die "Could not start the Console."
else
    docker run -d --name "$NAME" --restart unless-stopped -p "127.0.0.1:${PORT}:3000" \
        --add-host host.docker.internal:host-gateway \
        -e OPENFLOWS_MANAGER_URL="$(printf '%s' "$MANAGER_URL" | sed -E 's#//(127\.0\.0\.1|localhost)#//host.docker.internal#')" \
        "$IMAGE" >/dev/null \
        || rollback_and_die "Could not start the Console."
fi

CONSOLE="http://127.0.0.1:${PORT}"
printf 'Waiting for the Console'
for _ in $(seq 1 40); do
    curl -fs -m 3 -o /dev/null "${CONSOLE}/login" 2>/dev/null && break
    printf '.'; sleep 1
done
printf '\n'
curl -fs -m 3 -o /dev/null "${CONSOLE}/login" 2>/dev/null \
    || rollback_and_die "The Console did not start." "Check: docker logs ${NAME}"

# End to end: the sign-in start must reach the Manager and answer with a redirect.
headers="$(curl -s -m 10 -o /dev/null -D - "${CONSOLE}/auth/github/start" || true)"
status="$(printf '%s' "$headers" | head -n1 | awk '{print $2}')"
if [ "$status" != 307 ] && [ "$status" != 302 ]; then
    rollback_and_die "The Console started but its sign-in does not reach the Manager (HTTP ${status:-none})." \
"Check: docker logs ${NAME} --tail 10
Manager used: ${MANAGER_URL}"
fi

# The new Console works: the old ones are no longer needed.
for id in ${OLD[@]+"${OLD[@]}"}; do docker rm -f "$id" >/dev/null 2>&1 || true; done

location="$(printf '%s' "$headers" | tr -d '\r' | awk -F': ' 'tolower($1)=="location"{print $2}')"
parsed="$(python3 - "$location" <<'PY' 2>/dev/null || true
import sys, urllib.parse as u
p = u.urlparse(sys.argv[1])
q = u.parse_qs(p.query)
print(p.scheme + "://" + p.netloc)
print((q.get("redirect_uri") or [""])[0])
PY
)"
dest_origin="$(printf '%s\n' "$parsed" | sed -n 1p)"
callback="$(printf '%s\n' "$parsed" | sed -n 2p)"

# The sign-in cookie belongs to the host the browser used, and GitHub returns to the callback host.
# localhost and 127.0.0.1 are different hosts to a browser, so the address to open must match.
open_url="$CONSOLE"
case "$callback" in
    "http://localhost:${PORT}/auth/github/callback") open_url="http://localhost:${PORT}" ;;
    "http://127.0.0.1:${PORT}/auth/github/callback") open_url="http://127.0.0.1:${PORT}" ;;
esac

printf '\nConsole is running. Open exactly: %s\n' "$open_url"
if [ "$dest_origin" != "https://github.com" ]; then
    cat <<EOF

The Manager's sign-in did not redirect to GitHub (it answered with: ${location:-nothing}).
That is how the Manager from ./scripts/setup.sh works: it signs you in without GitHub OAuth,
through its own console at ${MANAGER_URL}. The Console cannot sign in against it. Use the
Manager's console, or run the operator setup (deployment.compose.yml) to sign in here.
EOF
elif [ -n "$callback" ] && [ "$open_url" = "$CONSOLE" ] && [ "$callback" != "${CONSOLE}/auth/github/callback" ]; then
    cat <<EOF

Heads up: after GitHub sign-in the browser is sent to
  ${callback}
not to this Console. Signing in through the Console works only when the Manager's
OPENFLOWS_PUBLIC_URL is http://localhost:${PORT} (or http://127.0.0.1:${PORT}) and the GitHub App
lists that address plus /auth/github/callback as a callback URL. Until then, use the Manager's
own console at ${MANAGER_URL}.
EOF
fi
