#!/usr/bin/env bash
# Check that a workspace image digest names an OpenFlows workspace image.
#
#   scripts/check-workspace-image.sh [DIGEST] [REPOSITORY]
#
# DIGEST defaults to OPENFLOWS_WORKSPACE_IMAGE_DIGEST, read from the environment
# or from .env.prod in the repository root. REPOSITORY defaults to
# OPENFLOWS_WORKSPACE_IMAGE_REPO, else ghcr.io/the-agenticflow/openflows-workspace,
# the repository the role templates run hosted workspaces from.
#
# Run it before starting a Manager on a release whose templates use the OpenFlows
# workspace image. Releases up to 1.8.2 took the digest of codercom/enterprise-base;
# such a digest does not exist in the workspace image repository, and every
# hosted workspace build would fail to pull its image.
set -euo pipefail

root="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
from_env_file() {
    [ -f "$root/.env.prod" ] || return 0
    sed -n "s/^$1=//p" "$root/.env.prod" | tail -1 | tr -d '"'"'"
}
digest="${1:-${OPENFLOWS_WORKSPACE_IMAGE_DIGEST:-$(from_env_file OPENFLOWS_WORKSPACE_IMAGE_DIGEST)}}"
repository="${2:-${OPENFLOWS_WORKSPACE_IMAGE_REPO:-$(from_env_file OPENFLOWS_WORKSPACE_IMAGE_REPO)}}"
repository="${repository:-ghcr.io/the-agenticflow/openflows-workspace}"

fail() {
    echo "error: $*" >&2
    exit 1
}
[ -n "$digest" ] || fail "no digest given and OPENFLOWS_WORKSPACE_IMAGE_DIGEST is not set"
printf '%s' "$digest" | grep -qE '^sha256:[0-9a-f]{64}$' ||
    fail "'$digest' is not an image digest (sha256: followed by 64 hexadecimal characters)"

reference="${repository}@${digest}"
if ! docker pull -q --platform linux/amd64 "$reference" >/dev/null 2>&1; then
    fail "$reference cannot be pulled. If this digest is from codercom/enterprise-base (releases up to 1.8.2), replace OPENFLOWS_WORKSPACE_IMAGE_DIGEST with the digest of ${repository} listed under \"Docker images\" in the release notes of the version you run."
fi
title="$(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.title"}}' "$reference")"
[ "$title" = openflows-workspace ] ||
    fail "$reference is not an OpenFlows workspace image (its title label is '${title}')"
version="$(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.version"}}' "$reference")"
runtime="$(docker run --rm --platform linux/amd64 --entrypoint cat "$reference" /opt/openflows/runtime/VERSION 2>/dev/null || echo unknown)"
echo "ok: $reference is the OpenFlows workspace image ${version} (runtime ${runtime}, linux/amd64)"
