#!/usr/bin/env bash
# Require a new runtime/VERSION when the workspace runtime changes.
#
#   BASE_SHA=<base> HEAD_SHA=<head> scripts/check-runtime-version.sh
#
# Release manifests record runtime/VERSION as the workspace runtime release, and
# a Manager can require a specific one (OPENFLOWS_WORKSPACE_RUNTIME_RELEASE). The
# record is only meaningful if every change to what the image runs changes it.
# Changes to the step signatures the templates call also need a new
# OF_RUNTIME_API in runtime/lib/bootstrap.sh and in the five templates.
#
# Compares the merge base of BASE_SHA and HEAD_SHA with HEAD_SHA. Without
# BASE_SHA (a push to a branch) there is nothing to compare, and it passes.
set -euo pipefail

if [ -z "${BASE_SHA:-}" ]; then
    echo "No base revision; skipping the runtime version check."
    exit 0
fi
head="${HEAD_SHA:-HEAD}"
base="$(git merge-base "$BASE_SHA" "$head")"
if ! git cat-file -e "${base}:runtime/VERSION" 2>/dev/null; then
    echo "The workspace runtime is new in this change; runtime/VERSION starts at $(git show "${head}:runtime/VERSION")."
    exit 0
fi
changed="$(git diff --name-only "$base" "$head" -- runtime/lib runtime/bin runtime/Dockerfile)"
if [ -z "$changed" ]; then
    echo "The workspace runtime did not change."
    exit 0
fi
before="$(git show "${base}:runtime/VERSION")"
after="$(git show "${head}:runtime/VERSION" 2>/dev/null || true)"
if [ "$before" = "$after" ]; then
    echo "error: the workspace runtime changed but runtime/VERSION is still ${before}:" >&2
    printf '  %s\n' $changed >&2
    echo "Raise runtime/VERSION. If the of_* steps the templates call changed, also raise OF_RUNTIME_API in runtime/lib/bootstrap.sh and the check in every template." >&2
    exit 1
fi
echo "The workspace runtime changed and runtime/VERSION moved from ${before} to ${after}."
