#!/usr/bin/env bash
# Resolves the Docker Publish version for a `workflow_run` trigger.
#
# release-plz creates the `openflows-X.Y.Z` tag DURING the triggering `Release`
# run, so the tag that a given run produced is the newest exact-version tag whose
# creator date is >= the run's start_at. We deliberately do NOT impose an upper
# bound on the creator date: GitHub's tag creator timestamp and the run's
# completed_at are recorded on different clocks, and the tag can appear to be
# created fractionally after completed_at, causing a strict [start, end] window
# to miss it. Omitting the upper bound is safe because the Release and Docker
# Publish workflows both carry a `concurrency` group that prevents a second
# release from running concurrently; if only one release tag can be pushed after
# start_at, selecting the newest-created tag >= start_at always identifies it.
# We purposefully do NOT gate on the run's head SHA: `workflow_run.head_sha`
# can be the `develop` tip (a commit that PREDATES the release commit), so an
# ancestor relationship is unreliable. A tagless run (no tag >= start_at)
# resolves to nothing, which lets the caller skip without rebuilding/republishing
# the previous release.
#
# Usage:
#   resolve_version <start_at> <end_at>
#
# Both timestamps are ISO-8601 (e.g. 2026-09-17T13:30:50Z) as supplied by
# github.event.workflow_run.created_at / completed_at. The end_at argument is
# accepted for interface compatibility but is not used as an upper bound.
# Reads tags from the current git repository (run from the checkout root after
# `git fetch --tags`).
#
# Prints the resolved version WITHOUT the `openflows-` prefix, or nothing (empty)
# when no release tag was created at or after start_at.
set -euo pipefail

resolve_version() {
  local start_at="$1"
  # end_at accepted for interface compatibility; not used as an upper bound
  # (see header comment for the rationale).
  local end_at="$2"
  local run_start_epoch t tag_epoch best_epoch="" version=""
  run_start_epoch="$(date -d "${start_at}" +%s)"
  for t in $(git tag --list 'openflows-*'); do
    # Only exact openflows-X.Y.Z tags are candidates; reject any tag with a
    # suffix (e.g. openflows-1.2.3-beta, openflows-1.2.3.4) up front.
    if ! printf '%s' "${t#openflows-}" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
      continue
    fi
    tag_epoch="$(git for-each-ref --format='%(creatordate:unix)' "refs/tags/${t}")"
    if [ -n "${tag_epoch}" ] && [ "${tag_epoch}" -ge "${run_start_epoch}" ]; then
      # Choose the newest tag created at or after start_at (by creator date),
      # not the highest version number. The concurrency guards on Release and
      # Docker Publish ensure only one release tag is pushed after start_at.
      if [ -z "${best_epoch}" ] || [ "${tag_epoch}" -gt "${best_epoch}" ]; then
        best_epoch="${tag_epoch}"
        version="${t#openflows-}"
      fi
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
