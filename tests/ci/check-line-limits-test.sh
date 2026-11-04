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

# ── Cleanup ─────────────────────────────────────────────────────────────────
# The EXIT trap removes the temp parent and every scratch repo under it.

if [ "$failures" -gt 0 ]; then
    printf '\n%d check(s) failed\n' "$failures"
    exit 1
fi
echo
echo "All line-limit checks passed."
