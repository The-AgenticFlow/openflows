#!/usr/bin/env bash
# Tests the default tenant name that scripts/setup.sh derives from owner/repo.
# The tenant workspace is openflows-nexus-NAME, so NAME must fit Coder's
# workspace name rule (at most 16 letters, digits and single hyphens), and
# different repos must not end up with the same default name (#490).
#
# Usage: bash tests/ci/setup-tenant-name-test.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# shellcheck source=../../scripts/setup.sh
source "${ROOT}/scripts/setup.sh"   # main only runs when setup.sh is executed, not sourced
set +e

failures=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }
expect_eq() {   # expect_eq description actual expected
    if [ "$2" = "$3" ]; then pass "$1"; else fail "$1 (got '$2', want '$3')"; fi
}

echo "Valid owner-repo names are kept as-is"
expect_eq "org/backend" "$(default_tenant_name org/backend)" "org-backend"
expect_eq "org/a-b" "$(default_tenant_name org/a-b)" "org-a-b"
expect_eq "abcdefgh/ijklmno (16 chars)" "$(default_tenant_name abcdefgh/ijklmno)" "abcdefgh-ijklmno"

echo "Names that had to change get a repo hash"
for repo in org/a.b org/a_b org/x--y Jagoum/rust-library The-AgenticFlow/openflows a.b/c_d__e; do
    name="$(default_tenant_name "$repo")"
    if [[ "$name" =~ -[0-9a-f]{6}$ ]]; then pass "$repo -> $name"; else fail "$repo -> $name has no hash suffix"; fi
done

echo "Every default name fits Coder's workspace name rule"
for repo in org/backend org/a.b org/a_b org/a-b org/x--y Jagoum/rust-library \
    The-AgenticFlow/openflows a.b/c_d__e _org/-repo- abcdefgh/ijklmnop; do
    name="$(default_tenant_name "$repo")"
    if tenant_name_ok "$name"; then pass "$repo -> $name (${#name})"; else fail "$repo -> '$name' is not a valid tenant name"; fi
done

echo "Repos that normalise alike get distinct names"
names="$(for repo in org/a.b org/a_b org/a-b org/a..b; do default_tenant_name "$repo"; echo; done)"
expect_eq "org/a.b, org/a_b, org/a-b, org/a..b" "$(printf '%s\n' "$names" | sort -u | wc -l | tr -d ' ')" "4"
names="$(for repo in The-AgenticFlow/openflows The-AgenticFlow/openflows-docs; do default_tenant_name "$repo"; echo; done)"
expect_eq "long names sharing a prefix" "$(printf '%s\n' "$names" | sort -u | wc -l | tr -d ' ')" "2"

echo "The same repo always gets the same name"
expect_eq "org/a.b twice" "$(default_tenant_name org/a.b)" "$(default_tenant_name org/a.b)"

echo "Explicit --name values are checked"
for bad in my_team my.team -team team- my--team waytoolongtenantname ""; do
    if tenant_name_ok "$bad"; then fail "'$bad' should be rejected"; else pass "rejects '$bad'"; fi
done
for good in rustlib ok-name A1 abcdefghijklmnop; do
    if tenant_name_ok "$good"; then pass "accepts '$good'"; else fail "'$good' should be accepted"; fi
done

if [ "$failures" -gt 0 ]; then
    printf '\n%d check(s) failed\n' "$failures"
    exit 1
fi
printf '\nAll checks passed\n'
