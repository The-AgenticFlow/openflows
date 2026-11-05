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

echo "Switching versions"
switch_to() {   # switch_to VERSION — run the release stage; echo what is installed afterwards
    (
        ROOT="${WORK}/switch"; ENV_FILE="${ROOT}/.env"; BIN_DIR="${ROOT}/.dev-binaries"; HOST_BIN_DIR="${BIN_DIR}/host"
        LOG_FILE="${WORK}/switch.log"; MODE=binary; REPO=o/r
        needs_host_programs() { return 1; }
        mkdir -p "$ROOT"; touch "$ENV_FILE"
        env_set OPENFLOWS_VERSION "$1"
        set -e
        stage_release >/dev/null 2>&1
    ) || echo "stopped"
    "${WORK}/switch/.dev-binaries/openflows"
}
harness() { "${WORK}/switch/.dev-binaries/openflows-harness"; }
publish 3.0.0 "$WORKSPACE_TARGET"
publish 3.1.0 "$WORKSPACE_TARGET"
expect_eq "install" "$(switch_to 3.0.0)" "openflows 3.0.0 ${WORKSPACE_TARGET}"
expect_eq "upgrade" "$(switch_to 3.1.0)" "openflows 3.1.0 ${WORKSPACE_TARGET}"
expect_eq "rollback" "$(switch_to 3.0.0)" "openflows 3.0.0 ${WORKSPACE_TARGET}"
expect_eq "a version without files stops and keeps the programs" "$(switch_to 2.9.9 | paste -sd' ' -)" "stopped openflows 3.0.0 ${WORKSPACE_TARGET}"
switch_to 3.0.0 >/dev/null
rm "${WORK}/switch/.dev-binaries/openflows-harness"
switch_to 3.0.0 >/dev/null
expect_eq "a deleted program is restored" "$(harness)" "openflows-harness 3.0.0 ${WORKSPACE_TARGET}"
printf '#!/bin/sh\necho damaged\n' >"${WORK}/switch/.dev-binaries/openflows-harness"
switch_to 3.0.0 >/dev/null
expect_eq "a damaged program is restored" "$(harness)" "openflows-harness 3.0.0 ${WORKSPACE_TARGET}"
printf '#!/bin/sh\necho openflows-harness 3.1.0\n' >"${WORK}/switch/.dev-binaries/openflows-harness"
switch_to 3.0.0 >/dev/null
expect_eq "a half-finished switch is repaired" "$(harness)" "openflows-harness 3.0.0 ${WORKSPACE_TARGET}"
rm "${WORK}"/switch/.dev-binaries/{openflows,openflows-doctor,openflows-harness,CHECKSUMS}
switch_to 3.0.0 >/dev/null
expect_eq "programs deleted with their checksums are restored" "$(harness)" "openflows-harness 3.0.0 ${WORKSPACE_TARGET}"
chmod -x "${WORK}/switch/.dev-binaries/openflows-harness"
switch_to 3.0.0 >/dev/null
expect_eq "a program that lost its permissions is restored" "$(harness)" "openflows-harness 3.0.0 ${WORKSPACE_TARGET}"
touch -t 202001010000 "${WORK}/switch/.dev-binaries/openflows"
switch_to 3.0.0 >/dev/null
expect_eq "intact programs are not downloaded again" "$(date -r "${WORK}/switch/.dev-binaries/openflows" +%Y)" 2020

echo "Machines with their own programs"
switch_host_to() {   # switch_host_to VERSION — like switch_to, on a machine that needs native programs
    (
        ROOT="${WORK}/switch-host"; ENV_FILE="${ROOT}/.env"; BIN_DIR="${ROOT}/.dev-binaries"; HOST_BIN_DIR="${BIN_DIR}/host"
        LOG_FILE="${WORK}/switch-host.log"; MODE=binary; REPO=o/r
        needs_host_programs() { return 0; }
        host_target() { echo fake-host; }
        mkdir -p "$ROOT"; touch "$ENV_FILE"
        env_set OPENFLOWS_VERSION "$1"
        set -e
        stage_release >/dev/null 2>&1
    ) || echo "stopped"
}
host_program() { "${WORK}/switch-host/.dev-binaries/host/openflows"; }
workspace_program() { "${WORK}/switch-host/.dev-binaries/openflows"; }
publish 4.0.0 "$WORKSPACE_TARGET"
publish 4.0.0 fake-host
publish 4.1.0 "$WORKSPACE_TARGET"
switch_host_to 4.0.0
expect_eq "installs native programs" "$(host_program)" "openflows 4.0.0 fake-host"
expect_eq "and the workspaces' programs" "$(workspace_program)" "openflows 4.0.0 ${WORKSPACE_TARGET}"
expect_eq "a failed native download stops" "$(switch_host_to 4.1.0)" "stopped"
expect_eq "and keeps the workspaces' programs" "$(workspace_program)" "openflows 4.0.0 ${WORKSPACE_TARGET}"
rm "${WORK}/switch-host/.dev-binaries/host/openflows"
switch_host_to 4.0.0
expect_eq "a deleted native program is restored" "$(host_program)" "openflows 4.0.0 fake-host"
printf '#!/bin/sh\necho damaged\n' >"${WORK}/switch-host/.dev-binaries/host/openflows"
switch_host_to 4.0.0
expect_eq "a damaged native program is restored" "$(host_program)" "openflows 4.0.0 fake-host"
rm "${WORK}"/switch-host/.dev-binaries/host/{openflows,openflows-doctor,openflows-harness,CHECKSUMS}
switch_host_to 4.0.0
expect_eq "native programs deleted with their checksums are restored" "$(host_program)" "openflows 4.0.0 fake-host"
chmod -x "${WORK}/switch-host/.dev-binaries/host/openflows"
switch_host_to 4.0.0
expect_eq "a native program that lost its permissions is restored" "$(host_program)" "openflows 4.0.0 fake-host"

echo "Image repository"
staged_image() {   # staged_image SHELL_REPO ENV_REPO — the image setup pulls and the digest it pins
    (
        ROOT="${WORK}/image-repo"; ENV_FILE="${ROOT}/.env"; BIN_DIR="${ROOT}/.dev-binaries"
        LOG_FILE="${WORK}/image-repo.log"; PULLED="${WORK}/image-repo.pulled"
        use_host_programs() { :; }
        mkdir -p "$ROOT"; rm -f "$ENV_FILE"; touch "$ENV_FILE"
        env_set OPENFLOWS_VERSION 5.0.0
        [ -z "$2" ] || env_set OPENFLOWS_IMAGE_REPO "$2"
        case "$1" in
            "") unset OPENFLOWS_IMAGE_REPO ;;
            empty) export OPENFLOWS_IMAGE_REPO="" ;;
            *) export OPENFLOWS_IMAGE_REPO="$1" ;;
        esac
        docker() {
            local image="${!#}"
            case "$1 $*" in
                pull*) printf '%s' "$image" >"$PULLED" ;;
                *RepoDigests*) printf '%s@sha256:abc\n' "${image%:*}" ;;
            esac
        }
        stage_image >/dev/null
        printf '%s %s' "$(cat "$PULLED")" "$(image_ref)"
    )
}
DEFAULT_IMAGE=ghcr.io/the-agenticflow/openflows
expect_eq "the default" "$(staged_image "" "")" "${DEFAULT_IMAGE}:5.0.0 ${DEFAULT_IMAGE}:5.0.0@sha256:abc"
expect_eq "from .env" "$(staged_image "" reg.test/env)" "reg.test/env:5.0.0 reg.test/env:5.0.0@sha256:abc"
expect_eq "the shell wins over .env, as in Compose" "$(staged_image reg.test/shell reg.test/env)" "reg.test/shell:5.0.0 reg.test/shell:5.0.0@sha256:abc"
expect_eq "an empty shell value means the default, as in Compose" "$(staged_image empty reg.test/env)" "${DEFAULT_IMAGE}:5.0.0 ${DEFAULT_IMAGE}:5.0.0@sha256:abc"

echo "Shared App without compiling"
expect_shared() {   # expect_shared VERSION yes|no
    if has_shared_app "$1"; then expect_eq "$1" yes "$2"; else expect_eq "$1" no "$2"; fi
}
expect_shared 1.8.1 no
expect_shared 1.7.0 no
expect_shared 1.8.2 yes
expect_shared 1.10.0 yes
files_for() {   # files_for MODE AUTH_MODE
    (
        ROOT="${WORK}/files-$RANDOM"; mkdir -p "$ROOT"; ENV_FILE="${ROOT}/.env"; MODE="$1"
        printf 'OPENFLOWS_GITHUB_AUTH_MODE=%s\n' "$2" >"$ENV_FILE"
        unset OPENFLOWS_GITHUB_AUTH_MODE COMPOSE_FILE
        compose_files | grep -v '^-f$' | paste -sd' ' -
    )
}
expect_eq "source, own App: plain compose" "$(files_for source self-managed)" ""
expect_eq "source, shared App: compiles the Manager" "$(files_for source connected-local)" \
    "docker-compose.yml docker-compose.connected-local.yml"
expect_eq "binary, own App: plain compose" "$(files_for binary self-managed)" ""
expect_eq "binary, shared App: Manager image" "$(files_for binary connected-local)" \
    "docker-compose.yml docker-compose.connected-local.yml docker-compose.manager-image.yml"
expect_eq "compose, own App: images" "$(files_for compose self-managed)" \
    "docker-compose.yml docker-compose.images.yml"
expect_eq "compose, shared App: images and Manager image" "$(files_for compose connected-local)" \
    "docker-compose.yml docker-compose.connected-local.yml docker-compose.manager-image.yml docker-compose.images.yml"

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
