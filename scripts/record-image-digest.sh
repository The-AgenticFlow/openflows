#!/usr/bin/env bash
# Record a published image digest in the matching GitHub Release's notes.
#
# Users pin and verify Compose installs against these digests: a tag such as
# `openflows:1.8.1` can be rebuilt and moved, a digest cannot. Release notes stay
# editable on immutable releases, so the digest is added after the image is pushed.
#
# Usage: scripts/record-image-digest.sh <version> <image> <digest>
#   e.g. scripts/record-image-digest.sh 1.8.1 ghcr.io/the-agenticflow/openflows sha256:...
# Requires: gh (authenticated, contents: write), python3, GITHUB_REPOSITORY.
set -euo pipefail

VERSION="${1:?version required}"
IMAGE="${2:?image required}"
DIGEST="${3:?digest required}"
REPO="${GITHUB_REPOSITORY:?GITHUB_REPOSITORY required}"
TAG="openflows-${VERSION}"

if ! printf '%s' "${DIGEST}" | grep -qE '^sha256:[0-9a-f]{64}$'; then
    echo "::error::Invalid digest for ${IMAGE}:${VERSION}: '${DIGEST}'"
    exit 1
fi

if ! gh release view "${TAG}" --repo "${REPO}" >/dev/null 2>&1; then
    echo "::notice::No release ${TAG}; digest for ${IMAGE}:${VERSION} not recorded."
    exit 0
fi

LINE="- \`${IMAGE}:${VERSION}\` → \`${DIGEST}\`"
NOTES_FILE="$(mktemp)"
trap 'rm -f "${NOTES_FILE}"' EXIT

# Callers serialize writes per release (a shared `release-notes-<version>`
# concurrency group), so a read-modify-write here cannot drop another image's line.
# Rerunning replaces this image's previous line instead of adding a second one.
# shellcheck disable=SC2016 # Python source; the backticks are literal Markdown.
gh release view "${TAG}" --repo "${REPO}" --json body --jq '.body // ""' |
    python3 -c '
import sys
image, line = sys.argv[1], sys.argv[2]
header = "### Docker images"
intro = "Pin a Compose install to an exact image with `OPENFLOWS_IMAGE_DIGEST`."
body = sys.stdin.read().rstrip()
head, _, after = body.partition(header)
# The section ends at the next heading; anything after it is kept unchanged.
lines = after.splitlines()
end = next((i for i, l in enumerate(lines) if i > 0 and l.startswith("#")), len(lines))
section, rest = lines[:end], "\n".join(lines[end:]).strip()
entries = [l for l in section
           if l.startswith("- `") and not l.startswith(f"- `{image}:")]
entries.append(line)
parts = [head.rstrip(), "\n".join([header, "", intro, "", *sorted(entries)]), rest]
print("\n\n".join(p for p in parts if p))
' "${IMAGE}" "${LINE}" >"${NOTES_FILE}"

gh release edit "${TAG}" --repo "${REPO}" --notes-file "${NOTES_FILE}" >/dev/null

if ! gh release view "${TAG}" --repo "${REPO}" --json body --jq '.body // ""' | grep -qF -- "${LINE}"; then
    echo "::error::Could not record ${IMAGE}:${VERSION} digest in ${TAG}."
    exit 1
fi
echo "Recorded ${IMAGE}:${VERSION} → ${DIGEST} in ${TAG}."
