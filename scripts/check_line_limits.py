#!/usr/bin/env python3
"""Enforce per-file line-count limits between two git revisions.

Rule (global cap with grandfathering):
    For every file added or modified between BASE_SHA and HEAD_SHA:
        new_line_count <= max(MAX_LINES, line_count_on_base)

A newly-added file has 0 lines on the base, so its cap is exactly MAX_LINES.
An existing file already over MAX_LINES is "grandfathered": it is capped at its
current size, so it can only shrink (and is nudged toward the cap).

This stops new bloat (new files over the cap) and stops existing files from
growing, without ever forcing a big refactor of files that already exceed the
cap today.

Scope: all tracked text files, excluding lockfiles, build/vendored output and
binary files (see IGNORED_PATTERNS below).

Usage (run from inside the git repository):
    BASE_SHA=<base> HEAD_SHA=<head> python3 scripts/check_line_limits.py
    MAX_LINES overrides the cap (default 1000).
    Local example: BASE_SHA=HEAD~1 HEAD_SHA=HEAD python3 scripts/check_line_limits.py

Exits 0 if every file is within its limit, 1 otherwise (printing each violation).
"""

import fnmatch
import os
import subprocess
import sys

MAX_LINES = int(os.environ.get("MAX_LINES", "1000"))
BASE_SHA = os.environ.get("BASE_SHA", "")
HEAD_SHA = os.environ.get("HEAD_SHA", "HEAD")

# Paths/globs allowed to be large (lockfiles, build/vendored output, generated
# artifacts). A trailing "/" means "any file under this directory".
IGNORED_PATTERNS = [
    "Cargo.lock",
    "package-lock.json",
    "pnpm-lock.yaml",
    "yarn.lock",
    "npm-shrinkwrap.json",
    "go.sum",
    "poetry.lock",
    "uv.lock",
    "Pipfile.lock",
    "*.min.js",
    "*.min.css",
    "*.map",
    "*.wasm",
    "*.o",
    "*.so",
    "*.dylib",
    "*.dll",
    "*.exe",
    "*.pb.go",
    "*.pb.ts",
    "target/",
    "dist/",
    "build/",
    "out/",
    "vendor/",
    "node_modules/",
    ".cargo/",
]


def blob(sha: str, path: str) -> bytes:
    """Return the raw bytes of a file at a revision (b"" if absent)."""
    try:
        result = subprocess.run(
            ["git", "cat-file", "blob", f"{sha}:{path}"],
            capture_output=True,
        )
    except OSError:
        return b""
    return result.stdout if result.returncode == 0 else b""


def line_count(sha: str, path: str) -> int:
    """Number of logical lines of a file at a revision (0 if absent)."""
    return len(blob(sha, path).splitlines())


def is_binary(sha: str, path: str) -> bool:
    """A file is treated as binary if a NUL byte appears in its first 8 KiB."""
    return b"\x00" in blob(sha, path)[:8192]


def is_ignored(path: str) -> bool:
    for pattern in IGNORED_PATTERNS:
        if pattern.endswith("/"):
            if path.startswith(pattern):
                return True
        elif fnmatch.fnmatch(path, pattern):
            return True
    return False


def check_file(base_path, head_path):
    """Return a violation string for a changed file, or None."""
    if not head_path or is_ignored(head_path):
        return None
    if is_binary(HEAD_SHA, head_path):
        return None  # binary files are skipped

    if base_path:
        base_count = line_count(BASE_SHA, base_path)
    else:
        base_count = 0  # brand-new file

    head_count = line_count(HEAD_SHA, head_path)
    allowed = max(MAX_LINES, base_count)

    if head_count <= allowed:
        return None

    if base_count == 0:
        reason = f"new file exceeds the {MAX_LINES}-line cap"
    elif base_count > MAX_LINES:
        reason = (
            f"grandfathered file ({base_count} lines on base) grew "
            "beyond its current size"
        )
    else:
        reason = f"file grew beyond the {MAX_LINES}-line cap"
    return f"FAIL  {head_path:<55} {head_count:>6} lines (limit {allowed}): {reason}"


def changed_files(base: str, head: str):
    """Yield (base_path, head_path) pairs for each added/modified file.

    base_path is None for brand-new files. Uses git rename detection so a
    rename keeps its grandfathered baseline.
    """
    result = subprocess.run(
        ["git", "diff", "-z", "--find-renames", "--name-status", base, head],
        capture_output=True,
        text=True,
    )
    fields = result.stdout.split("\x00")
    i = 0
    while i < len(fields):
        status = fields[i]
        i += 1
        if not status:
            continue
        if status.startswith("R"):
            old = fields[i]
            new = fields[i + 1]
            i += 2
            yield old, new
        elif status.startswith("C"):
            # Copy: treat the new path as its own file (cap = MAX_LINES).
            new = fields[i]
            i += 1
            yield None, new
        elif status in ("A", "M", "T"):
            path = fields[i]
            i += 1
            yield (None if status == "A" else path), path
        else:  # D (delete), U, X, B ... consume the path and move on
            i += 1


def main() -> int:
    if not BASE_SHA:
        print(
            "BASE_SHA not set (not a pull_request run); "
            "skipping line-limit check (PRs only)."
        )
        return 0

    print(
        f"Checking per-file line limits between {BASE_SHA} and {HEAD_SHA} "
        f"(cap {MAX_LINES})..."
    )

    violations = []
    for base_path, head_path in changed_files(BASE_SHA, HEAD_SHA):
        violation = check_file(base_path, head_path)
        if violation:
            violations.append(violation)

    if violations:
        print("\n".join(violations))
        print(
            f"\n{len(violations)} file(s) exceed their line-count limit.\n"
            "Refactor rather than grow: split large files into focused modules."
        )
        return 1

    print("OK: all changed files are within their line-count limits.")
    return 0


if __name__ == "__main__":
    sys.exit(main())
