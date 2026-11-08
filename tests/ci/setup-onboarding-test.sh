#!/usr/bin/env bash
# First-run setup invariants for the shared GitHub App mode.
set -euo pipefail
REPO_ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=../../scripts/setup.sh
source "$REPO_ROOT/scripts/setup.sh"
WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ENV_FILE="$WORK/.env"
ROOT="$REPO_ROOT"
MODE=source
printf 'CODER_SESSION_TOKEN=\n' >"$ENV_FILE"
[ "$(compose_files | tr '\n' ' ')" = '-f docker-compose.yml -f docker-compose.connected-local.yml ' ]
[ "$(MODE=binary; compose_files | tr '\n' ' ')" = '-f docker-compose.yml -f docker-compose.connected-local.yml -f docker-compose.manager-image.yml ' ]
[ "$(MODE=compose; compose_files | tr '\n' ' ')" = '-f docker-compose.yml -f docker-compose.connected-local.yml -f docker-compose.manager-image.yml -f docker-compose.images.yml ' ]
! rg -q '^CODER_EXTERNAL_AUTH_0_' "$REPO_ROOT/.env.example"
! rg -q '^OPENFLOWS_GITHUB_AUTH_MODE=' "$REPO_ROOT/.env.example"
bash -c 'set -euo pipefail; set -a; source "$1"' _ "$REPO_ROOT/.env.example"
rg -q 'stage_manager_coder_oauth' "$REPO_ROOT/scripts/setup.sh"
rg -q 'OPENFLOWS_CONNECTED_LOCAL_ENABLED' "$REPO_ROOT/scripts/setup.sh"
echo 'All shared App onboarding tests passed'
