#!/usr/bin/env bash
# Tests how scripts/setup.sh picks, verifies and installs release programs (#484),
# with a local folder standing in for the GitHub release.
#
# Usage: bash tests/ci/setup-binary-test.sh

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

WORK="$(mktemp -d)"
trap 'rm -r -- "$WORK"' EXIT
FAKE_RELEASE="${WORK}/release"
mkdir -p "$FAKE_RELEASE"

# The stand-in for downloads: the last argument is the URL, -o names the output.
curl() {
    local out="" arg
    while [ $# -gt 1 ]; do
        if [ "$1" = -o ]; then out="$2"; shift; fi
        shift
    done
    arg="${FAKE_RELEASE}/${1##*/}"
    [ -f "$arg" ] || return 22
    cp "$arg" "$out"
}
publish() {   # publish VERSION TARGET — add an archive and its checksum to the fake release
    local name="openflows-${1}-${2}" dir="${WORK}/build/openflows-${1}-${2}" bin
    mkdir -p "$dir"
    for bin in openflows openflows-doctor openflows-harness; do
        printf '#!/bin/sh\necho "%s %s %s"\n' "$bin" "$1" "$2" >"${dir}/${bin}"
    done
    tar -czf "${FAKE_RELEASE}/${name}.tar.gz" -C "${WORK}/build" "$name"
    (cd "$FAKE_RELEASE" && printf '%s  %s\n' "$(sha256_of "${name}.tar.gz")" "${name}.tar.gz" >"${name}.tar.gz.sha256")
}

echo "Release target for each machine"
target_for() {   # target_for OS ARCH GLIBC
    (
        uname() { if [ "$1" = -s ]; then echo "$FAKE_OS"; else echo "$FAKE_ARCH"; fi; }
        getconf() { [ -n "$FAKE_GLIBC" ] && echo "glibc $FAKE_GLIBC"; }
        FAKE_OS="$1" FAKE_ARCH="$2" FAKE_GLIBC="${3:-}"
        host_target
    )
}
expect_eq "Linux x86-64" "$(target_for Linux x86_64 2.35)" x86_64-unknown-linux-gnu
expect_eq "Linux x86-64 without glibc" "$(target_for Linux x86_64)" x86_64-unknown-linux-musl
expect_eq "Linux ARM64" "$(target_for Linux aarch64 2.35)" aarch64-unknown-linux-gnu
expect_eq "macOS Intel" "$(target_for Darwin x86_64)" x86_64-apple-darwin
expect_eq "macOS Apple Silicon" "$(target_for Darwin arm64)" aarch64-apple-darwin
expect_eq "unsupported machine" "$(target_for Linux riscv64 2.35)" ""

echo "Machines that need their own programs"
needs_for() {   # needs_for MODE TARGET GLIBC
    (
        MODE="$1" FAKE_TARGET="$2" FAKE_GLIBC="$3"
        host_target() { echo "$FAKE_TARGET"; }
        getconf() { [ -n "$FAKE_GLIBC" ] && echo "glibc $FAKE_GLIBC"; }
        if needs_host_programs; then echo yes; else echo no; fi
    )
}
expect_eq "compose, Linux x86-64, new glibc" "$(needs_for compose x86_64-unknown-linux-gnu 2.35)" no
expect_eq "compose, Linux x86-64, old glibc" "$(needs_for compose x86_64-unknown-linux-gnu 2.31)" yes
expect_eq "binary, Linux x86-64, old glibc" "$(needs_for binary x86_64-unknown-linux-gnu 2.31)" no
expect_eq "compose, macOS" "$(needs_for compose aarch64-apple-darwin "")" yes
expect_eq "binary, Linux ARM64" "$(needs_for binary aarch64-unknown-linux-gnu 2.35)" yes

echo "Installing an archive"
publish 2.0.0 x86_64-unknown-linux-gnu
DEST="${WORK}/dest"
install_archive 2.0.0 x86_64-unknown-linux-gnu "$DEST"
expect_eq "installs" "$?" 0
expect_eq "installs the right program" "$("${DEST}/openflows-harness")" "openflows-harness 2.0.0 x86_64-unknown-linux-gnu"
expect_eq "leaves no temporary files" "$(find "$DEST" -name '*.tmp' | wc -l | tr -d ' ')" 0

install_archive 1.8.1 x86_64-unknown-linux-gnu "${WORK}/none"
expect_eq "a release without files is reported as missing" "$?" 1
expect_eq "and installs nothing" "$([ -e "${WORK}/none" ] && echo yes || echo no)" no

publish 2.0.1 x86_64-unknown-linux-gnu
printf 'tampered' >>"${FAKE_RELEASE}/openflows-2.0.1-x86_64-unknown-linux-gnu.tar.gz"
install_archive 2.0.1 x86_64-unknown-linux-gnu "$DEST"
expect_eq "a wrong checksum is refused" "$?" 2
expect_eq "and keeps the installed programs" "$("${DEST}/openflows")" "openflows 2.0.0 x86_64-unknown-linux-gnu"

echo "Choosing the version"
version_with() {   # version_with ENV_VALUE KIT_VALUE LATEST
    (
        ROOT="${WORK}/kit-$RANDOM"; mkdir -p "$ROOT"; ENV_FILE="${ROOT}/.env"
        printf 'OPENFLOWS_VERSION=%s\n' "$1" >"$ENV_FILE"
        [ -z "$2" ] || printf '%s\n' "$2" >"${ROOT}/RELEASE_VERSION"
        FAKE_LATEST="$3"
        latest_release_version() { echo "$FAKE_LATEST"; }
        resolve_version
    )
}
expect_eq ".env wins" "$(version_with 1.9.0 2.0.0 3.0.0)" 1.9.0
expect_eq "then the setup kit" "$(version_with "" 2.0.0 3.0.0)" 2.0.0
expect_eq "then the latest release" "$(version_with "" "" 3.0.0)" 3.0.0

echo "prod.sh runs the installed programs"
prod_bin() {   # prod_bin MODE HAS_HOST
    (
        P="${WORK}/prod-$RANDOM"; mkdir -p "${P}/scripts" "${P}/.dev-binaries"
        cp "${ROOT}/scripts/prod.sh" "${P}/scripts/"
        printf 'OPENFLOWS_SETUP_MODE=%s\n' "$1" >"${P}/.env"
        printf '#!/bin/sh\necho workspace "$@"\n' >"${P}/.dev-binaries/openflows"
        chmod +x "${P}/.dev-binaries/openflows"
        if [ "$2" = yes ]; then
            mkdir -p "${P}/.dev-binaries/host"
            printf '#!/bin/sh\necho host "$@"\n' >"${P}/.dev-binaries/host/openflows"
            chmod +x "${P}/.dev-binaries/host/openflows"
        fi
        env -u OPENFLOWS_BIN bash "${P}/scripts/prod.sh" doctor 2>&1 | grep -E '^(workspace|host) ' | cut -d' ' -f1
    )
}
expect_eq "binary, same machine" "$(prod_bin binary no)" workspace
expect_eq "binary, macOS or ARM64" "$(prod_bin binary yes)" host
expect_eq "compose, macOS or ARM64" "$(prod_bin compose yes)" host

echo
if [ "$failures" -gt 0 ]; then
    echo "$failures test(s) failed"
    exit 1
fi
echo "All release install tests passed"
