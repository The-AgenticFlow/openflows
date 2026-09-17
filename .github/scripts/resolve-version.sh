#!/usr/bin/env bash
# Resolves the Docker Publish version for a `workflow_run` trigger.
#
# release-plz creates the `openflows-X.Y.Z` tag DURING the triggering `Release`
# run, so the tag that a given run produced is the newest exact-version tag whose
# creator date falls within that run's `[start_at, end_at]` window. We purposefully
# do NOT gate on the run's head SHA: `workflow_run.head_sha` can be the `develop`
# tip (a commit that PREDATES the release commit), so an ancestor relationship is
# unreliable. The creator-date window is the authoritative signal that this run
# created the tag; a tagless run (no new tag in the window) resolves to nothing,
# which lets the caller skip without rebuilding/republishing the previous release.
#
# Usage:
#   resolve_version <start_at> <end_at>
#
# Both timestamps are ISO-8601 (e.g. 2026-09-17T13:30:50Z) as supplied by
# github.event.workflow_run.created_at / completed_at. Reads tags from the current
# git repository (run from the checkout root after `git fetch --tags`).
#
# Prints the resolved version WITHOUT the `openflows-` prefix, or nothing (empty)
# when no release tag was created within the window.
set -euo pipefail

resolve_version() {
  local start_at="$1"
  local end_at="$2"
  local run_start_epoch run_end_epoch t tag_epoch version=""
  run_start_epoch="$(date -d "${start_at}" +%s)"
  run_end_epoch="$(date -d "${end_at}" +%s)"
  for t in $(git tag --list 'openflows-*' --sort=-v:refname); do
    # Only exact openflows-X.Y.Z tags are candidates; reject any tag with a
    # suffix (e.g. openflows-1.2.3-beta, openflows-1.2.3.4) up front.
    if ! printf '%s' "${t#openflows-}" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
      continue
    fi
    tag_epoch="$(git for-each-ref --format='%(creatordate:unix)' "refs/tags/${t}")"
    if [ -n "${tag_epoch}" ] && [ "${tag_epoch}" -ge "${run_start_epoch}" ] && [ "${tag_epoch}" -le "${run_end_epoch}" ]; then
      version="${t#openflows-}"
      break
    fi
  done
  printf '%s' "${version}"
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
  if [ "$#" -ne 2 ]; then
    echo "usage: $0 <start_at> <end_at>" >&2
    exit 2
  fi
  resolve_version "$1" "$2"
  echo
fi
