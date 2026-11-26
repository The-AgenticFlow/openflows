#!/usr/bin/env bash
# Build the OpenFlows workspace image from this checkout with locally built programs.
#
#   runtime/build-image.sh TAG PROGRAMS_DIR [VERSION]
#
# PROGRAMS_DIR holds linux/amd64 `openflows` and `openflows-harness` (for example
# .dev-binaries after `make dev-sync`). VERSION labels the image (default: dev).
# Releases build the same Dockerfile with the published openflows image as
# PROGRAMS_IMAGE instead (.github/workflows/docker-publish.yml).
set -euo pipefail

tag="${1:?usage: runtime/build-image.sh TAG PROGRAMS_DIR [VERSION]}"
programs="${2:?usage: runtime/build-image.sh TAG PROGRAMS_DIR [VERSION]}"
version="${3:-dev}"
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"

stage="$(mktemp -d)"
programs_image="openflows-workspace-programs:$(basename "$stage" | tr '[:upper:]' '[:lower:]')"
cleanup() {
    rm -rf "$stage"
    docker image rm "$programs_image" >/dev/null 2>&1 || true
}
trap cleanup EXIT
for program in openflows openflows-harness; do
    if [ ! -x "$programs/$program" ]; then
        echo "error: $programs/$program is missing or not executable" >&2
        exit 1
    fi
    cp "$programs/$program" "$stage/$program"
done

# A throwaway image holding only the programs stands in for the published openflows
# image, so the same Dockerfile builds with BuildKit and with the legacy builder.
printf 'FROM scratch\nCOPY openflows openflows-harness /usr/local/bin/\n' >"$stage/Dockerfile"
docker build --platform linux/amd64 -q -t "$programs_image" "$stage" >/dev/null

docker build --platform linux/amd64 \
    --build-arg "PROGRAMS_IMAGE=$programs_image" \
    --build-arg "OPENFLOWS_VERSION=$version" \
    -t "$tag" -f "$here/Dockerfile" "$here"
