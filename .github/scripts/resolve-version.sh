#!/usr/bin/env bash
# Resolves the Docker Publish version for a `workflow_run` trigger.
#
# release-plz creates the `openflows-X.Y.Z` tag DURING the triggering `Release`
# run, so the tag that a given run produced is the newest exact-version tag whose
# creator date falls within the run's `[start_at, end_at + CLOCK_SKEW_GRACE]`
# window. We deliberately do NOT gate on the run's head SHA: `workflow_run.head_sha`
# can be the `develop` tip (a commit that PREDATES the release commit), so an
# ancestor relationship is unreliable. The creator-date window is the authoritative
# signal that this run created the tag.
#
# An upper bound is REQUIRED, not optional: the Release and Docker Publish
# workflows use SEPARATE `concurrency` groups, so a subsequent Release run can
# push a newer tag while an earlier Docker Publish `resolve` job is still running.
# With an open upper bound that later tag would win and the earlier job would
# publish the wrong (newer) release. We therefore bound selection to
# `end_at + CLOCK_SKEW_GRACE` and select the newest-created tag in that window.
#
# The grace period absorbs clock skew: GitHub's tag creator timestamp and the
# run's completed_at are recorded on different clocks, and the tag can appear to
# be created fractionally AFTER completed_at. A strict `[start, end]` window
# would miss it (the original production bug); tolerating a small, fixed grace
# keeps the window bounded (so a later release cannot win) while still capturing
# the tag. The Release concurrency group serializes runs (a later run only starts
# after this one fully completes), so a subsequent release's tag is always created
# well beyond CLOCK_SKEW_GRACE from this run's completed_at and is safely excluded.
# A tagless run (no new tag in the window) resolves to nothing, which lets the
# caller skip without rebuilding/republishing the previous release.
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

# Seconds to extend the resolved window past completed_at to absorb cross-clock
# skew between the tag creator date and the run's completed_at. Smaller than any
# real inter-release gap (Release runs are serialized by a concurrency group and
# take minutes), so a subsequent release's tag stays outside the window.
CLOCK_SKEW_GRACE=60

resolve_version() {
  local start_at="$1"
  local end_at="$2"
  local run_start_epoch run_end_epoch t tag_epoch best_epoch="" version=""
  run_start_epoch="$(date -d "${start_at}" +%s)"
  run_end_epoch="$(( $(date -d "${end_at}" +%s) + CLOCK_SKEW_GRACE ))"
  for t in $(git tag --list 'openflows-*'); do
    # Only exact openflows-X.Y.Z tags are candidates; reject any tag with a
    # suffix (e.g. openflows-1.2.3-beta, openflows-1.2.3.4) up front.
    if ! printf '%s' "${t#openflows-}" | grep -qE '^[0-9]+\.[0-9]+\.[0-9]+$'; then
      continue
    fi
    tag_epoch="$(git for-each-ref --format='%(creatordate:unix)' "refs/tags/${t}")"
    if [ -n "${tag_epoch}" ] && [ "${tag_epoch}" -ge "${run_start_epoch}" ] && [ "${tag_epoch}" -le "${run_end_epoch}" ]; then
      # Choose the newest tag CREATED within the window (by its creator date),
      # not the highest version number. release-plz creates exactly one release
      # tag per run, so the run's tag is the newest-created in-window tag; an
      # unrelated higher-version or later-release tag outside the window must
      # not be selected.
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
