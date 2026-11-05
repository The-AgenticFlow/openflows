#!/usr/bin/env bash
# Tests scripts/check_line_limits.py against scratch git repositories.
#
# Usage: bash tests/ci/check-line-limits-test.sh

set -euo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="$ROOT/scripts/check_line_limits.py"

failures=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }

# All scratch repos live under one temp parent, removed on exit.
PARENT="$(mktemp -d)"
trap 'rm -rf "$PARENT"' EXIT

# ── Helpers ────────────────────────────────────────────────────────────────

new_repo() {
    local repo
    repo="$(mktemp -d "$PARENT/repo.XXXXXX")"
    git -C "$repo" init -q
    git -C "$repo" config user.email test@example.com
    git -C "$repo" config user.name test
    printf '%s' "$repo"
}

# gen_lines <file> <n> — write a text file with exactly n lines.
# Each file gets distinct content (its own name as a prefix) so git's rename
# detection never mispairs two unrelated files in the fixtures.
gen_lines() {
    local file="$1" n="$2" seed
    seed="$(basename "$file")"
    awk -v seed="$seed" -v n="$n" 'BEGIN{ for (i=1; i<=n; i++) printf "%s %d\n", seed, i }' > "$file"
}

# gen_binary_with_lines <file> <n> — write a BINARY file (contains NUL bytes)
# with n newline-terminated lines, so it would exceed the cap if not skipped.
gen_binary_with_lines() {
    python3 -c "import sys; n=int('$2'); sys.stdout.buffer.write(b'x\x00y\n' * n)" > "$1"
}

commit() { # <repo> <message> -> echoes the new commit SHA
    local repo="$1" msg="$2"
    git -C "$repo" add -A
    git -C "$repo" commit -qm "$msg"
    git -C "$repo" rev-parse HEAD
}

# run_check <repo> <base_sha> <head_sha> -> echoes the checker's stdout+stderr
run_check() {
    local repo="$1" base="$2" head="$3"
    ( cd "$repo" && BASE_SHA="$base" HEAD_SHA="$head" python3 "$SCRIPT" )
}

# expect_violation <desc> <repo> <base> <head> <file-that-must-appear> [files-that-must-not-appear...]
# Requires the checker to BOTH print a violation AND exit non-zero, so broken
# enforcement (a checker that prints but returns 0) is caught.
expect_violation() {
    local desc="$1" repo="$2" base="$3" head="$4" must="$5"; shift 5
    local out code
    set +e
    out="$(run_check "$repo" "$base" "$head" 2>&1)"
    code=$?
    set -e
    if [ "$code" -eq 0 ]; then
        fail "$desc (expected a violation, but the checker exited 0)"
        printf '      output:\n%s\n' "$out" | sed 's/^/      /'
        return
    fi
    if [[ "$out" != *"FAIL"* ]]; then
        fail "$desc (expected a FAIL in output)"
        printf '      output:\n%s\n' "$out" | sed 's/^/      /'
        return
    fi
    if [[ "$out" != *"$must"* ]]; then
        fail "$desc (expected '$must' in output)"
        printf '      output:\n%s\n' "$out" | sed 's/^/      /'
        return
    fi
    local forbidden
    for forbidden in "$@"; do
        if [[ "$out" == *"$forbidden"* ]]; then
            fail "$desc ('$forbidden' should NOT be flagged)"
            printf '      output:\n%s\n' "$out" | sed 's/^/      /'
            return
        fi
    done
    pass "$desc"
}

# expect_clean <desc> <repo> <base> <head>
expect_clean() {
    local desc="$1" repo="$2" base="$3" head="$4"
    local out
    out="$(run_check "$repo" "$base" "$head" 2>&1)" || {
        fail "$desc (expected clean, got violations)"
        printf '      output:\n%s\n' "$out" | sed 's/^/      /'
        return
    }
    pass "$desc"
}

# expect_error <desc> <repo> <base> <head> — the checker must fail (non-zero)
# and surface a git error rather than silently passing (fail-closed).
expect_error() {
    local desc="$1" repo="$2" base="$3" head="$4"
    local out code
    set +e
    out="$(run_check "$repo" "$base" "$head" 2>&1)"
    code=$?
    set -e
    if [ "$code" -eq 0 ]; then
        fail "$desc (expected a non-zero exit, got 0)"
        printf '      output:\n%s\n' "$out" | sed 's/^/      /'
        return
    fi
    if [[ "$out" != *"ERROR"* ]]; then
        fail "$desc (expected an ERROR in output)"
        printf '      output:\n%s\n' "$out" | sed 's/^/      /'
        return
    fi
    pass "$desc"
}

# ── Scenario 1: new + modified files vs the cap ────────────────────────────
repo="$(new_repo)"
gen_lines "$repo/small.rs" 500
gen_lines "$repo/grandfathered.rs" 1500
gen_lines "$repo/Cargo.lock" 2000        # ignored lockfile
gen_binary_with_lines "$repo/data.bin" 100  # binary (never changed -> untouched)
gen_lines "$repo/to_delete.rs" 50
base1="$(commit "$repo" base)"

gen_lines "$repo/new_small.rs" 800       # A: within cap
gen_lines "$repo/new_large.rs" 1500      # A: over cap -> violation
gen_lines "$repo/small.rs" 900           # M: still <= 1000
gen_lines "$repo/grandfathered.rs" 1400  # M: shrank below base 1500
gen_lines "$repo/Cargo.lock" 2500        # ignored, still fine
gen_binary_with_lines "$repo/new_data.bin" 2000  # A: binary -> skipped
mkdir -p "$repo/tools/example"
gen_lines "$repo/tools/example/Cargo.lock" 2000  # A: nested lockfile -> ignored
rm "$repo/to_delete.rs"                  # D: ignore
head1="$(commit "$repo" head)"

echo "Scenario 1: new files and modified files against the cap"
expect_violation "new file over 1000 lines is caught" "$repo" "$base1" "$head1" "new_large.rs" \
    "new_small.rs" "small.rs" "grandfathered.rs" "Cargo.lock" "data.bin" \
    "new_data.bin" "tools/example/Cargo.lock" "to_delete.rs"

# ── Scenario 2: growth of an existing file past the cap ────────────────────
repo="$(new_repo)"
gen_lines "$repo/ok.rs" 400
gen_lines "$repo/grand.rs" 1500
base2="$(commit "$repo" base)"

gen_lines "$repo/ok.rs" 1100   # grew from 400 to 1100 -> over cap, violation
gen_lines "$repo/grand.rs" 900 # grandfathered shrank below 1000 -> now just a normal file
head2="$(commit "$repo" head)"

echo "Scenario 2: an existing file grown past 1000 is caught"
expect_violation "existing file grown past 1000 is caught" "$repo" "$base2" "$head2" "ok.rs" "grand.rs"

# ── Scenario 3: grandfathered file must not grow ───────────────────────────
repo="$(new_repo)"
gen_lines "$repo/grand.rs" 1500
base3="$(commit "$repo" base)"

gen_lines "$repo/grand.rs" 1600   # grew beyond its own base size -> violation
head3="$(commit "$repo" head)"

echo "Scenario 3: a grandfathered file cannot grow beyond its current size"
expect_violation "grandfathered file growth is caught" "$repo" "$base3" "$head3" "grand.rs"

# ── Scenario 4: renames preserve the grandfathered baseline ────────────────
repo="$(new_repo)"
gen_lines "$repo/grand.rs" 1500
base4="$(commit "$repo" base)"

git -C "$repo" mv grand.rs moved.rs
head4="$(commit "$repo" rename)"

echo "Scenario 4: renaming a grandfathered file keeps its baseline"
expect_clean "rename of a grandfathered file passes" "$repo" "$base4" "$head4"

# ── Scenario 5: boundary at exactly the cap ────────────────────────────────
repo="$(new_repo)"
gen_lines "$repo/empty.rs" 0
base5="$(commit "$repo" base)"

gen_lines "$repo/new.rs" 1000   # exactly at the cap -> allowed
head5="$(commit "$repo" head)"

echo "Scenario 5: boundary at exactly the cap"
expect_clean "a file at exactly 1000 lines passes" "$repo" "$base5" "$head5"

# ── Scenario 6: base branch advancing must not flag unrelated PRs ──────────
repo="$(new_repo)"
gen_lines "$repo/f.rs" 1500
common="$(commit "$repo" common)"
git -C "$repo" branch develop
git -C "$repo" checkout -q -b feature
gen_lines "$repo/feature_only.rs" 10
feature_head="$(commit "$repo" feature)"     # f.rs untouched (1500)
git -C "$repo" checkout -q develop
gen_lines "$repo/f.rs" 900
develop_head="$(commit "$repo" develop-shrink)"  # develop shrank f.rs to 900

echo "Scenario 6: an advancing base branch does not flag an unrelated PR"
expect_clean "unrelated PR passes when the base branch shrank a grandfathered file" \
    "$repo" "$develop_head" "$feature_head"

# ── Scenario 7: markdown files are excluded ─────────────────────────────────
repo="$(new_repo)"
gen_lines "$repo/CHANGELOG.md" 500
base7="$(commit "$repo" base)"

gen_lines "$repo/CHANGELOG.md" 1500   # M: grows a lot -> would violate if counted
gen_lines "$repo/new.md" 2000          # A: new big markdown -> would violate if counted
head7="$(commit "$repo" head)"

echo "Scenario 7: markdown files are excluded from the cap"
expect_clean "markdown files are not subject to the cap" "$repo" "$base7" "$head7"

# ── Scenario 8: boundary just over the cap ──────────────────────────────────
repo="$(new_repo)"
gen_lines "$repo/placeholder.rs" 1
base8="$(commit "$repo" base)"

gen_lines "$repo/over.rs" 1001         # A: one line over the cap -> violation
head8="$(commit "$repo" head)"

echo "Scenario 8: a new file one line over the cap is caught"
expect_violation "a new file at 1001 lines is caught" "$repo" "$base8" "$head8" "over.rs"

# ── Scenario 9: git failure fails closed ────────────────────────────────────
repo="$(new_repo)"
gen_lines "$repo/a.rs" 10
base9="$(commit "$repo" base)"

echo "Scenario 9: a git failure fails the check instead of passing"
expect_error "invalid BASE_SHA exits non-zero with an error" "$repo" "DEADBEEF-not-a-commit" "HEAD"

# ── Scenario 10: nested excluded directories are ignored ───────────────────
repo="$(new_repo)"
mkdir -p "$repo/src"
gen_lines "$repo/src/main.rs" 10
base10="$(commit "$repo" base)"

mkdir -p "$repo/crates/x/target"
gen_lines "$repo/crates/x/target/generated.rs" 2000  # nested target/ -> ignored
head10="$(commit "$repo" head)"

echo "Scenario 10: excluded directories are matched at any depth"
expect_clean "a file under a nested excluded directory passes" "$repo" "$base10" "$head10"

# ── Scenario 11: rename from an ignored path is treated as new in-scope ────
repo="$(new_repo)"
mkdir -p "$repo/vendor" "$repo/src"
gen_lines "$repo/vendor/large.rs" 1001  # over cap, but in an ignored dir
gen_lines "$repo/src/placeholder.rs" 1
base11="$(commit "$repo" base)"

git -C "$repo" mv vendor/large.rs src/large.rs  # now in-scope -> must meet the cap
head11="$(commit "$repo" rename)"

echo "Scenario 11: a rename out of an ignored path applies the cap"
expect_violation "a file renamed from an ignored path is capped as new" \
    "$repo" "$base11" "$head11" "src/large.rs"

# ── Cleanup ─────────────────────────────────────────────────────────────────
# The EXIT trap removes the temp parent and every scratch repo under it.

if [ "$failures" -gt 0 ]; then
    printf '\n%d check(s) failed\n' "$failures"
    exit 1
fi
echo
echo "All line-limit checks passed."
