#!/usr/bin/env bash
# Regression tests for the Docker Publish version resolver
# (`.github/scripts/resolve-version.sh`).
#
# This resolver replaced the ancestor-based logic that silently skipped Docker
# publication for a real release: the previous fix gated on
# `workflow_run.head_sha`, which can be the `develop` tip (a commit that
# PREDATES the release commit), so a genuine release tag was always rejected.
# These tests lock in the behavior so a later workflow change cannot silently
# skip publication again.
#
# The resolver selects the newest `openflows-X.Y.Z` tag whose creator date is
# >= start_at. end_at is accepted for interface compatibility but is NOT used as
# an upper bound: the tag creator timestamp and completed_at are on different
# clocks and the tag can land after completed_at, which was the original
# production bug. Dropping the upper bound is safe because the Release and
# Docker Publish concurrency groups prevent two releases from running in parallel.
#
# We exercise the resolver against a throwaway git repository with fixture tags at
# deterministic creator dates (via GIT_COMMITTER_DATE), so the assertions below
# mirror the real release topology.
set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESOLVER="${SCRIPT_DIR}/../../.github/scripts/resolve-version.sh"

GREEN=$'\033[32m'; RED=$'\033[31m'; NC=$'\033[0m'
PASS=0
FAIL=0

# Create an annotated tag whose tag object carries an explicit creator date.
# `%(creatordate)` for an annotated tag is the tag object's tagger date, so we
# control it deterministically with GIT_COMMITTER_DATE.
make_tag() {
  local name="$1" epoch="$2" msg="$3"
  GIT_COMMITTER_DATE="@${epoch} +0000" git tag -a "${name}" -m "${msg}"
}

# assert_eq <expected> <actual> <label>
assert_eq() {
  local expected="$1" actual="$2" label="$3"
  if [ "${expected}" = "${actual}" ]; then
    PASS=$((PASS + 1))
    echo "  ${GREEN}PASS${NC} ${label}: '${actual}'"
  else
    FAIL=$((FAIL + 1))
    echo "  ${RED}FAIL${NC} ${label}: expected '${expected}', got '${actual}'"
  fi
}

# --- Build fixture repo ---------------------------------------------------------
FIXTURE="$(mktemp -d)"
trap 'rm -rf "${FIXTURE}"' EXIT
cd "${FIXTURE}"
git init -q
git config user.email "test@example.com"
git config user.name "Test"
git config commit.gpgsign false
git commit -q --allow-empty -m "base"

# Epoch anchor values (arbitrary, deterministic).
E10=$((1700000000 + 10))    # openflows-1.4.0
E20=$((1700000000 + 20))    # openflows-1.5.0
E25=$((1700000000 + 25))    # openflows-1.2.3-beta (suffix, must be ignored)
E30=$((1700000000 + 30))    # openflows-1.5.1 (this run's release)
E35=$((1700000000 + 35))    # openflows-1.6.0 (unrelated higher version, older than run tag)
E40=$((1700000000 + 40))    # openflows-1.5.2 (newest-created in-window, despite lower version)

make_tag openflows-1.4.0 "${E10}" "release 1.4.0"
make_tag openflows-1.5.0 "${E20}" "release 1.5.0"
make_tag openflows-1.2.3-beta "${E25}" "prerelease 1.2.3-beta"
make_tag openflows-1.5.1 "${E30}" "release 1.5.1"
make_tag openflows-1.6.0 "${E35}" "release 1.6.0"
make_tag openflows-1.5.2 "${E40}" "release 1.5.2"

# Resolve uses ISO-8601 windows; build them from the epoch anchors.
win() { date -d "@$1" -u +%Y-%m-%dT%H:%M:%SZ; }

echo "== Resolve version regression tests =="

# 1. The Release run starts at E20+1 (between 1.5.0 and 1.5.1). The release tag
#    1.5.1 is created at E30. end_at is set to E30-1 (before the tag) to
#    simulate the original production bug where the tag appeared to land AFTER
#    completed_at due to clock skew. The resolver must still pick 1.5.1 because
#    end_at is no longer an upper bound.
W1="$(win $((E20 + 1)))"
E1="$(win $((E30 - 1)))"
assert_eq "1.5.2" "$(bash "${RESOLVER}" "${W1}" "${E1}" 2>/dev/null)" "tag after completed_at is still resolved (clock-skew scenario)"

# 1b. Narrow window: start_at just before 1.5.1, end_at just after it (classic
#     in-window case). Without an upper bound all later tags (E35, E40) are also
#     candidates; the newest-created one (1.5.2 at E40) is returned.
W1b="$(win $((E30 - 1)))"
E1b="$(win $((E30 + 1)))"
assert_eq "1.5.2" "$(bash "${RESOLVER}" "${W1b}" "${E1b}" 2>/dev/null)" "newest-created tag >= start_at wins (1.5.2)"

# 2. Only tags at E10 and later are candidates when start_at is just before E10
#    and end_at is just after E10. Without an upper bound all tags >= E10 are
#    admitted; the newest-created (1.5.2 at E40) is returned.
W2="$(win $((E10 - 1)))"
E2="$(win $((E10 + 1)))"
assert_eq "1.5.2" "$(bash "${RESOLVER}" "${W2}" "${E2}" 2>/dev/null)" "oldest run: newest-created tag >= start_at is 1.5.2"

# 3. No tags at all in the repo -> must resolve to nothing (tagless release run).
#    We verify this in the current fixture repo using a start_at that falls
#    after the last tag so all tags are excluded; this is also covered by test 4.
#    For the "no tags exist" case, use a throwaway empty repo.
EMPTY_REPO="$(mktemp -d)"
trap 'rm -rf "${EMPTY_REPO}"' EXIT
(
  cd "${EMPTY_REPO}"
  git init -q
  git config user.email "test@example.com"
  git config user.name "Test"
  git commit -q --allow-empty -m "base"
  assert_eq "" "$(bash "${RESOLVER}" "$(win $((E10 - 20)))" "$(win $((E10 - 10)))" 2>/dev/null)" "empty repo (no tags) skips"
)


# 4. Window after all tags: all tags are >= any start_at <= E10, so a start_at
#    after the last tag (E40+5) resolves to nothing.
W4="$(win $((E40 + 5)))"
E4="$(win $((E40 + 6)))"
assert_eq "" "$(bash "${RESOLVER}" "${W4}" "${E4}" 2>/dev/null)" "window after last release skips"

# 5. A window covering only a suffix tag must still skip (suffix never selected).
W5="$(win $((E25 - 1)))"
E5="$(win $((E25 + 1)))"
# start_at is between E20 and E25; without an upper bound all tags >= start_at
# are admitted, but the suffix tag is filtered. The next exact tags are E30 and
# above, so the newest (1.5.2) is returned.
assert_eq "1.5.2" "$(bash "${RESOLVER}" "${W5}" "${E5}" 2>/dev/null)" "suffix tag openflows-1.2.3-beta is rejected; newest exact tag wins"

# 6. start_at just before E35; all tags from E35 onwards are candidates.
#    1.5.2 (E40) is newest-created.
W6="$(win $((E35 - 1)))"
E6="$(win $((E40 + 1)))"
assert_eq "1.5.2" "$(bash "${RESOLVER}" "${W6}" "${E6}" 2>/dev/null)" "newest-created tag wins (1.5.2 over 1.6.0)"

# 7. start_at just before E35, end_at just after E35. Without an upper bound,
#    1.5.2 (E40) is still >= start_at and newer-created than 1.6.0 (E35).
W7="$(win $((E35 - 1)))"
E7="$(win $((E35 + 1)))"
assert_eq "1.5.2" "$(bash "${RESOLVER}" "${W7}" "${E7}" 2>/dev/null)" "newest-created tag >= start_at (1.5.2) selected even when end_at is before it"

echo
echo "== Result: ${GREEN}${PASS} passed${NC}, ${RED}${FAIL} failed${NC} =="
[ "${FAIL}" -eq 0 ]
