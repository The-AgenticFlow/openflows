#!/usr/bin/env bash
# Tests scripts/check-runtime-version.sh and scripts/check-template-image-repository.sh.
#
# Usage: bash tests/ci/check-runtime-version-test.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
failures=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }
expect() { # expect NAME STATUS COMMAND...
    local name="$1" want="$2" got=0
    shift 2
    "$@" >/dev/null 2>&1 || got=$?
    if [ "$got" = "$want" ]; then pass "$name"; else fail "$name (exit $got, want $want)"; fi
}

PARENT="$(mktemp -d)"
trap 'rm -rf "$PARENT"' EXIT
repo="$PARENT/repo"
git init -q "$repo"
git -C "$repo" config user.email test@example.com
git -C "$repo" config user.name test
commit() { git -C "$repo" add -A && git -C "$repo" commit -qm "$1" && git -C "$repo" rev-parse HEAD; }
check() { (cd "$repo" && BASE_SHA="$1" HEAD_SHA="$2" bash "$ROOT/scripts/check-runtime-version.sh"); }

echo "Runtime version"
mkdir -p "$repo/runtime/lib" "$repo/docs"
echo old >"$repo/docs/a.md"
before_runtime=$(commit "before the runtime")
echo 'step() { :; }' >"$repo/runtime/lib/bootstrap.sh"
echo 1.0.0 >"$repo/runtime/VERSION"
introduced=$(commit "introduce the runtime")
expect "a new runtime needs no bump" 0 check "$before_runtime" "$introduced"
echo new >"$repo/docs/a.md"
docs=$(commit "docs only")
expect "changes outside the runtime need no bump" 0 check "$introduced" "$docs"
echo 'step() { echo changed; }' >"$repo/runtime/lib/bootstrap.sh"
unbumped=$(commit "change the runtime")
expect "a runtime change without a new VERSION fails" 1 check "$docs" "$unbumped"
echo 1.1.0 >"$repo/runtime/VERSION"
bumped=$(commit "bump")
expect "a runtime change with a new VERSION passes" 0 check "$docs" "$bumped"
expect "no base revision passes" 0 check "" "$bumped"

echo "Template image repository"
templates="$PARENT/templates"
for role in forge nexus; do
    mkdir -p "$templates/openflows-$role"
    printf 'variable "workspace_image_repository" {\n  type    = string\n  default = "ghcr.io/owner/openflows-workspace"\n}\n' \
        >"$templates/openflows-$role/main.tf"
done
expect "templates that name the pinned repository pass" 0 \
    bash "$ROOT/scripts/check-template-image-repository.sh" "$templates" ghcr.io/owner/openflows-workspace
expect "a fork's repository fails" 1 \
    bash "$ROOT/scripts/check-template-image-repository.sh" "$templates" ghcr.io/fork/openflows-workspace
expect "the real templates name the published repository" 0 \
    bash "$ROOT/scripts/check-template-image-repository.sh" "$ROOT/crates/coder-client/templates" \
    ghcr.io/the-agenticflow/workspace-runtime
expect "a fork's prefix fails against the real templates" 1 \
    bash "$ROOT/scripts/check-template-image-repository.sh" "$ROOT/crates/coder-client/templates" \
    ghcr.io/fork/workspace-runtime
expect "an empty templates folder fails" 1 \
    bash "$ROOT/scripts/check-template-image-repository.sh" "$PARENT/none" ghcr.io/owner/openflows-workspace

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "All checks passed"
