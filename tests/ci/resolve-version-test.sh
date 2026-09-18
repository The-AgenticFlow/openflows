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
# The resolver selects the newest `openflows-X.Y.Z` tag whose creator date falls
# within the run's `[start_at, end_at + CLOCK_SKEW_GRACE]` window. The grace
# absorbs the tag-appears-after-completed_at clock-skew bug, and the upper bound
# is required because Release and Docker Publish use SEPARATE concurrency groups
# (a later Release can push a newer tag while an earlier Docker Publish resolve
# is still running; with an open upper bound that later tag would wrongly win).
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
cd "${FIXTURE}"
git init -q
git config user.email "test@example.com"
git config user.name "Test"
git config commit.gpgsign false
git commit -q --allow-empty -m "base"

# Epoch anchor values (arbitrary, deterministic). Spacing reflects real releases:
# distinct releases are minutes/hundreds of seconds apart, far greater than the
# resolver's 60-second clock-skew grace.
BASE=1700000000
T_A=$((BASE + 10))      # openflows-1.4.0 (old release)
T_BETA=$((BASE + 50))   # openflows-1.2.3-beta (suffix, must be ignored)
T_HI_OLD=$((BASE + 150)) # openflows-1.6.0 (higher version, created BEFORE this run -> excluded)
T_B=$((BASE + 200))     # openflows-1.5.0 (previous release)
T_RUN=$((BASE + 300))   # openflows-1.5.1 (THIS run's release)
T_LATER=$((BASE + 600)) # openflows-1.5.2 (a LATER release, beyond the grace -> excluded)

make_tag openflows-1.4.0 "${T_A}" "release 1.4.0"
make_tag openflows-1.2.3-beta "${T_BETA}" "prerelease 1.2.3-beta"
make_tag openflows-1.6.0 "${T_HI_OLD}" "release 1.6.0"
make_tag openflows-1.5.0 "${T_B}" "release 1.5.0"
make_tag openflows-1.5.1 "${T_RUN}" "release 1.5.1"
make_tag openflows-1.5.2 "${T_LATER}" "release 1.5.2"

# Resolve uses ISO-8601 windows; build them from the epoch anchors.
win() { date -d "@$1" -u +%Y-%m-%dT%H:%M:%SZ; }

echo "== Resolve version regression tests =="

# 1. Clock-skew scenario (the original production bug): the run starts just
#    before the release tag and "completes" BEFORE the tag is created (the tag
#    appears to land fractionally after completed_at). The grace extends the
#    window past completed_at, so 1.5.1 is still selected; the later 1.5.2 tag is
#    far beyond the grace and must NOT win.
W1="$(win $((T_RUN - 5)))"
E1="$(win $((T_RUN - 1)))"
assert_eq "1.5.1" "$(bash "${RESOLVER}" "${W1}" "${E1}" 2>/dev/null)" "tag created just after completed_at still resolves (clock skew), not the later tag"

# 2. Later-release exclusion (review finding): even when the earlier Docker
#    Publish resolve might run late, a tag from a SUBSEQUENT release (1.5.2,
#    well beyond the grace) is outside the window and must not be selected.
#    The run's own tag 1.5.1 wins.
W2="$(win $((T_RUN - 5)))"
E2="$(win $((T_RUN + 5)))"
assert_eq "1.5.1" "$(bash "${RESOLVER}" "${W2}" "${E2}" 2>/dev/null)" "a later release's tag beyond the grace is excluded; run's 1.5.1 wins"

# 3. An older higher-version tag (1.6.0, created before this run started) is
#    outside the window and must not be chosen just because it is the highest
#    version. The run's in-window tag 1.5.1 wins.
W3="$(win $((T_B + 10)))"
E3="$(win $((T_RUN + 5)))"
assert_eq "1.5.1" "$(bash "${RESOLVER}" "${W3}" "${E3}" 2>/dev/null)" "older higher-version 1.6.0 is outside the window; newest in-window 1.5.1 wins"

# 4. A window covering only the previous release tag resolves it, ignoring the
#    suffix tag that sits in the same broad window.
W4="$(win $((T_A - 1)))"
E4="$(win $((T_A + 1)))"
assert_eq "1.4.0" "$(bash "${RESOLVER}" "${W4}" "${E4}" 2>/dev/null)" "prior-release window resolves 1.4.0"

# 5. A window covering only the suffix tag must resolve to nothing (suffix never
#    selected, and no exact tag leaks into the window).
W5="$(win $((T_BETA - 2)))"
E5="$(win $((T_BETA + 2)))"
assert_eq "" "$(bash "${RESOLVER}" "${W5}" "${E5}" 2>/dev/null)" "suffix tag openflows-1.2.3-beta is rejected (no in-window exact tag)"

# 6. Empty repo (no tags at all) -> must resolve to nothing (tagless release run).
EMPTY_REPO="$(mktemp -d)"
trap 'rm -rf "${FIXTURE}" "${EMPTY_REPO}"' EXIT
(
  cd "${EMPTY_REPO}"
  git init -q
  git config user.email "test@example.com"
  git config user.name "Test"
  git commit -q --allow-empty -m "base"
  assert_eq "" "$(bash "${RESOLVER}" "$(win $((T_A - 1)))" "$(win $((T_A + 1)))" 2>/dev/null)" "empty repo (no tags) skips"
)

# 7. Window after all tags -> must resolve to nothing.
W7="$(win $((T_LATER + 5)))"
E7="$(win $((T_LATER + 10)))"
assert_eq "" "$(bash "${RESOLVER}" "${W7}" "${E7}" 2>/dev/null)" "window after last release skips"

echo
echo "== Result: ${GREEN}${PASS} passed${NC}, ${RED}${FAIL} failed${NC} =="
[ "${FAIL}" -eq 0 ]
