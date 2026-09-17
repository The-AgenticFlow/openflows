#!/usr/bin/env bash
# Regression tests for the Docker Publish version resolver
# (`.github/scripts/resolve-version.sh`).
#
# This resolver (the creator-date window selection) replaced the ancestor-based
# logic that silently skipped Docker publication for a real release: the previous
# fix gated on `workflow_run.head_sha`, which can be the `develop` tip (a commit
# that PREDATES the release commit), so a genuine release tag was always rejected.
# These tests lock in the behavior so a later workflow change cannot silently
# skip publication again.
#
# We exercise the resolver against a throwaway git repository with fixture tags at
# deterministic creator dates (via GIT_COMMITTER_DATE), so the assertions below
# mirror the real release topology:
#   - a release tag whose commit is a DESCENDANT of the triggering head (covered
#     by testing the resolver with only the creator-date window, no head gate),
#   - exact-version matching that rejects suffix tags,
#   - tagless windows that must resolve to nothing (so the caller skips).
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

make_tag openflows-1.4.0 "${E10}" "release 1.4.0"
make_tag openflows-1.5.0 "${E20}" "release 1.5.0"
make_tag openflows-1.2.3-beta "${E25}" "prerelease 1.2.3-beta"
make_tag openflows-1.5.1 "${E30}" "release 1.5.1"

# Resolve uses ISO-8601 windows; build them from the epoch anchors.
win() { date -d "@$1" -u +%Y-%m-%dT%H:%M:%SZ; }

echo "== Resolve version regression tests =="

# 1. Release window containing exactly tag 1.5.1 -> must pick it (descendant case,
#    no head gate; this was the previously-broken path).
W1="$(win $((E30 - 1)))"
E1="$(win $((E30 + 1)))"
assert_eq "1.5.1" "$(bash "${RESOLVER}" "${W1}" "${E1}" 2>/dev/null)" "release window resolves newest in-window tag (1.5.1)"

# 2. Only 1.4.0 falls inside this window -> newest in-window is 1.4.0.
W2="$(win $((E10 - 1)))"
E2="$(win $((E10 + 1)))"
assert_eq "1.4.0" "$(bash "${RESOLVER}" "${W2}" "${E2}" 2>/dev/null)" "older release window picks 1.4.0"

# 3. Window before any tag (tagless release) -> empty (skip publication).
W3="$(win $((E10 - 20)))"
E3="$(win $((E10 - 10)))"
assert_eq "" "$(bash "${RESOLVER}" "${W3}" "${E3}" 2>/dev/null)" "tagless (pre-release) window skips"

# 4. Window after all tags (tagless) -> empty.
W4="$(win $((E30 + 5)))"
E4="$(win $((E30 + 6)))"
assert_eq "" "$(bash "${RESOLVER}" "${W4}" "${E4}" 2>/dev/null)" "window after last release skips"

# 5. A window covering a suffix tag only must still skip (suffix never selected).
W5="$(win $((E25 - 1)))"
E5="$(win $((E25 + 1)))"
assert_eq "" "$(bash "${RESOLVER}" "${W5}" "${E5}" 2>/dev/null)" "suffix tag openflows-1.2.3-beta is rejected"

echo
echo "== Result: ${GREEN}${PASS} passed${NC}, ${RED}${FAIL} failed${NC} =="
[ "${FAIL}" -eq 0 ]
