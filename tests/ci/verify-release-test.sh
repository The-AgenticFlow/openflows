#!/usr/bin/env bash
# Tests scripts/verify-release.sh (#526) against fake release files: each broken file
# must be caught and named, and a good release must pass.
#
# Usage: bash tests/ci/verify-release-test.sh

# The stand-in registry functions are called by the sourced script, not directly, and
# the sourced script keeps its own failure count inside image_case's subshell.
# shellcheck disable=SC2317,SC2031
set -uo pipefail

ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
SCRIPT="${ROOT}/scripts/verify-release.sh"

failures=0
pass() { printf '  ok    %s\n' "$1"; }
fail() { printf '  FAIL  %s\n' "$1"; failures=$((failures + 1)); }

WORK="$(mktemp -d)"
trap 'rm -rf -- "$WORK"' EXIT
sha256_of() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'; }

# Fake programs are shell scripts, so `file` and `objdump` are replaced by stand-ins that
# read a description from the program's second line: `# type=<file -b output> glibc=<x.y>`.
mkdir -p "${WORK}/bin"
cat >"${WORK}/bin/file" <<'EOF'
#!/bin/sh
sed -n 's/^# type=\(.*\) glibc=.*/\1/p' "$2"
EOF
cat >"${WORK}/bin/objdump" <<'EOF'
#!/bin/sh
g="$(sed -n 's/^# type=.* glibc=\(.*\)/\1/p' "$2")"
[ -n "$g" ] && printf '0000 DF *UND* 0000 GLIBC_2.2.5 memcpy\n0000 DF *UND* 0000 GLIBC_%s read\n' "$g"
EOF
chmod +x "${WORK}/bin/file" "${WORK}/bin/objdump"
export PATH="${WORK}/bin:${PATH}"

GNU_TYPE='ELF 64-bit LSB pie executable, x86-64, version 1 (SYSV), dynamically linked'

# archive VERSION TARGET [TYPE] [GLIBC] [PRINTED_VERSION] [EXIT_STATUS]: build a fake archive and its checksum.
archive() {
    local version="$1" target="$2" type="${3:-$GNU_TYPE}" glibc="${4:-2.17}" printed="${5:-$1}" status="${6:-0}"
    local name="openflows-${version}-${target}" dir bin
    dir="${WORK}/build/${name}"
    rm -rf -- "$dir"; mkdir -p "$dir"
    for bin in openflows openflows-doctor openflows-harness; do
        printf '#!/bin/sh\n# type=%s glibc=%s\n' "$type" "$glibc" >"${dir}/${bin}"
        if [ "$bin" = openflows ]; then printf 'echo "openflows %s"\nexit %s\n' "$printed" "$status" >>"${dir}/${bin}"; fi
        chmod +x "${dir}/${bin}"
    done
    mkdir -p "${WORK}/dist"
    tar -czf "${WORK}/dist/${name}.tar.gz" -C "${WORK}/build" "$name"
    (cd "${WORK}/dist" && printf '%s  %s\n' "$(sha256_of "${name}.tar.gz")" "${name}.tar.gz" >"${name}.tar.gz.sha256")
    printf '%s' "${WORK}/dist/${name}.tar.gz"
}

# expect_result DESCRIPTION 0|1 TEXT COMMAND...: the exit status, and TEXT in the output.
expect_result() {
    local description="$1" status="$2" text="$3" out rc
    shift 3
    out="$("$@" 2>&1)"; rc=$?
    if [ "$rc" != "$status" ]; then fail "${description} (exit ${rc}, want ${status})"; printf '%s\n' "$out" | sed 's/^/        /'; return; fi
    if [ -n "$text" ] && ! printf '%s' "$out" | grep -qF -- "$text"; then
        fail "${description} (no '${text}' in output)"; printf '%s\n' "$out" | sed 's/^/        /'; return
    fi
    pass "$description"
}

T=x86_64-unknown-linux-gnu
echo "Program archives"
f="$(archive 2.0.0 "$T")"
expect_result "a good archive passes" 0 "openflows --version prints 2.0.0" bash "$SCRIPT" archive "$f" 2.0.0 "$T"

f="$(archive 2.0.1 "$T")"
printf 'x' >>"$f"
expect_result "a changed archive fails its checksum" 1 "FAIL openflows-2.0.1-${T}.tar.gz: checksum does not match" bash "$SCRIPT" archive "$f" 2.0.1 "$T"

f="$(archive 2.0.2 "$T")"
rm "${f}.sha256"
expect_result "a missing checksum file fails" 1 "missing openflows-2.0.2-${T}.tar.gz.sha256" bash "$SCRIPT" archive "$f" 2.0.2 "$T"

f="$(archive 2.0.3 "$T")"
printf '%s  other.tar.gz\n' "$(sha256_of "$f")" >"${f}.sha256"
expect_result "a checksum file naming another file fails" 1 "checksum file names 'other.tar.gz'" bash "$SCRIPT" archive "$f" 2.0.3 "$T"

f="$(archive 2.0.4 "$T" "$GNU_TYPE" 2.17 2.0.3)"
expect_result "programs from another version fail" 1 "printed 'openflows 2.0.3', not 'openflows 2.0.4'" bash "$SCRIPT" archive "$f" 2.0.4 "$T"

f="$(archive 2.0.41 "$T" "$GNU_TYPE" 2.17 2.0.41 1)"
expect_result "a program that prints the version but fails" 1 "openflows --version exited with status 1" bash "$SCRIPT" archive "$f" 2.0.41 "$T"

f="$(archive 2.0.5 "$T" 'ELF 64-bit LSB pie executable, ARM aarch64, dynamically linked')"
expect_result "programs for another CPU fail" 1 "openflows is not built for ${T}" bash "$SCRIPT" archive "$f" 2.0.5 "$T"

f="$(archive 2.0.6 "$T" "$GNU_TYPE" 2.34)"
expect_result "programs needing a newer glibc fail" 1 "needs glibc 2.34, newer than 2.17" bash "$SCRIPT" archive "$f" 2.0.6 "$T"

f="$(archive 2.0.7 x86_64-unknown-linux-musl 'ELF 64-bit LSB executable, x86-64, version 1 (SYSV), static-pie linked' "")"
expect_result "a static musl archive passes without a glibc check" 0 "openflows-harness runs" bash "$SCRIPT" archive "$f" 2.0.7 x86_64-unknown-linux-musl

f="$(archive 2.0.8 x86_64-unknown-linux-musl "$GNU_TYPE")"
expect_result "a dynamically linked musl archive fails" 1 "openflows is not built for x86_64-unknown-linux-musl" bash "$SCRIPT" archive "$f" 2.0.8 x86_64-unknown-linux-musl

f="$(archive 2.0.9 "$T")"
name="openflows-2.0.9-${T}"
rm "${WORK}/build/${name}/openflows-harness"
tar -czf "$f" -C "${WORK}/build" "$name"
(cd "${WORK}/dist" && printf '%s  %s\n' "$(sha256_of "${name}.tar.gz")" "${name}.tar.gz" >"${name}.tar.gz.sha256")
expect_result "a missing program fails" 1 "openflows-harness is missing or not executable" bash "$SCRIPT" archive "$f" 2.0.9 "$T"

f="$(archive 2.1.0 "$T")"
expect_result "an archive checked against the wrong version fails" 1 "expected the name openflows-2.1.1-${T}.tar.gz" bash "$SCRIPT" archive "$f" 2.1.1 "$T"
expect_result "an unknown target fails" 1 "unknown target riscv64" bash "$SCRIPT" archive "$f" 2.1.0 riscv64

echo "Setup kit"
# kit VERSION: a setup kit packed the way release-assets.yml packs it, from this checkout.
# The paths release-assets.yml packs into the kit, read from its `tar` command so this test
# follows the workflow. RELEASE_VERSION is written by the workflow, so it is left out here.
KIT_PATHS="$(awk '/-setup\.tar\.gz" --transform/ {on=1; next} on {print; if ($0 !~ /\\$/) exit}' \
    "${ROOT}/.github/workflows/release-assets.yml" | sed 's/\\$//' | tr -s ' \n' ' ' | sed 's/RELEASE_VERSION//')"
[ -n "$KIT_PATHS" ] || { echo "could not read the kit's files from release-assets.yml"; exit 1; }
kit() {
    local version="$1" tag="openflows-${1}" dir
    dir="${WORK}/kit/${tag}"
    rm -rf -- "${WORK}/kit"; mkdir -p "$dir"
    # shellcheck disable=SC2086 # the paths include a glob, as in the workflow
    (cd "$ROOT" && tar -cf - $KIT_PATHS) | tar -xf - -C "$dir"
    printf '%s\n' "$version" >"${dir}/RELEASE_VERSION"
}
pack_kit() { # pack_kit VERSION: archive the kit folder and write its checksum
    local tag="openflows-${1}"
    tar -czf "${WORK}/dist/${tag}-setup.tar.gz" -C "${WORK}/kit" "$tag"
    (cd "${WORK}/dist" && sha256_of "${tag}-setup.tar.gz" | awk -v n="${tag}-setup.tar.gz" '{print $1"  "n}' >"${tag}-setup.tar.gz.sha256")
    printf '%s' "${WORK}/dist/${tag}-setup.tar.gz"
}
kit 3.0.0
expect_result "the kit this checkout produces passes" 0 "RELEASE_VERSION is 3.0.0" bash "$SCRIPT" kit "$(pack_kit 3.0.0)" 3.0.0

kit 3.0.1; printf '3.0.0\n' >"${WORK}/kit/openflows-3.0.1/RELEASE_VERSION"
expect_result "a kit with another version fails" 1 "RELEASE_VERSION is '3.0.0', not 3.0.1" bash "$SCRIPT" kit "$(pack_kit 3.0.1)" 3.0.1

kit 3.0.2; rm "${WORK}/kit/openflows-3.0.2/scripts/setup-release.sh"
expect_result "a kit missing a script fails" 1 "missing scripts/setup-release.sh" bash "$SCRIPT" kit "$(pack_kit 3.0.2)" 3.0.2

kit 3.0.3; rm "${WORK}/kit/openflows-3.0.3/docker-compose.manager-image.yml"
expect_result "a kit missing a Compose file setup uses fails" 1 "missing docker-compose.manager-image.yml, which setup uses" bash "$SCRIPT" kit "$(pack_kit 3.0.3)" 3.0.3

kit 3.0.4; rm -r "${WORK}/kit/openflows-3.0.4/deploy"
expect_result "a kit missing files Compose mounts fails" 1 "which docker-compose.yml mounts" bash "$SCRIPT" kit "$(pack_kit 3.0.4)" 3.0.4

kit 3.0.5; printf 'if then\n' >>"${WORK}/kit/openflows-3.0.5/scripts/prod.sh"
expect_result "a kit with a broken script fails" 1 "scripts/prod.sh has a syntax error" bash "$SCRIPT" kit "$(pack_kit 3.0.5)" 3.0.5

kit 3.0.7; rm "${WORK}/kit/openflows-3.0.7/scripts/setup-docker.sh"
expect_result "a kit missing a script setup.sh sources fails" 1 "missing scripts/setup-docker.sh, which setup.sh sources" bash "$SCRIPT" kit "$(pack_kit 3.0.7)" 3.0.7

kit 3.0.8; chmod -x "${WORK}/kit/openflows-3.0.8/scripts/prod.sh"
expect_result "a kit whose prod.sh is not executable fails" 1 "scripts/prod.sh is not executable" bash "$SCRIPT" kit "$(pack_kit 3.0.8)" 3.0.8

kit 3.0.9; chmod -x "${WORK}/kit/openflows-3.0.9/scripts/setup.sh"
expect_result "a kit whose setup.sh is not executable fails" 1 "scripts/setup.sh is not executable" bash "$SCRIPT" kit "$(pack_kit 3.0.9)" 3.0.9

kit 3.0.6; f="$(pack_kit 3.0.6)"; printf 'x' >>"$f"
expect_result "a changed kit fails its checksum" 1 "checksum does not match" bash "$SCRIPT" kit "$f" 3.0.6

echo "Image labels"
# image_case VERSION_LABEL REVISION PLATFORMS: verify_image against a stand-in registry.
image_case() {
    (
        # shellcheck source=scripts/verify-release.sh
        source "$SCRIPT"
        FAKE_LABEL="$1" FAKE_REVISION="$2" FAKE_PLATFORMS="$3"
        registry_token() { echo token; }
        registry_get() {
            case "$2" in
                */manifests/4.0.0)
                    printf '{"manifests":['
                    local first=1 p
                    for p in $FAKE_PLATFORMS unknown/unknown; do
                        [ "$first" = 1 ] || printf ','; first=0
                        printf '{"digest":"sha256:%s","platform":{"os":"%s","architecture":"%s"}}' "${p//\//-}" "${p%/*}" "${p#*/}"
                    done
                    printf ']}' ;;
                */manifests/sha256:*) printf '{"config":{"digest":"sha256:cfg-%s"}}' "${2##*sha256:}" ;;
                */blobs/sha256:cfg-*)
                    local p="${2##*cfg-}"
                    printf '{"os":"%s","architecture":"%s","config":{"Labels":{"org.opencontainers.image.version":"%s","org.opencontainers.image.revision":"%s"}}}' \
                        "${p%%-*}" "${p#*-}" "$FAKE_LABEL" "$FAKE_REVISION" ;;
                *) return 22 ;;
            esac
        }
        tree_of() { case "$1" in tagged|merge) echo tree-of-tag ;; *) echo other-tree ;; esac; }
        main image ghcr.io/o/openflows 4.0.0 tree-of-tag
    )
}
expect_result "an image built from the tag passes" 0 "linux/arm64 revision label tagged has the tag's source" image_case 4.0.0 tagged "linux/amd64 linux/arm64"
expect_result "a revision with the same source passes" 0 "linux/amd64 is labelled 4.0.0" image_case 4.0.0 merge "linux/amd64"
expect_result "an image labelled with another version fails" 1 "linux/amd64 is labelled '3.9.0', not 4.0.0" image_case 3.9.0 tagged "linux/amd64"
expect_result "a revision label from a rebuild is reported, not failed" 0 "revision label elsewhere is not the tag's source" image_case 4.0.0 elsewhere "linux/amd64"
expect_result "a missing revision label is reported, not failed" 0 "has no revision label" image_case 4.0.0 "" "linux/amd64"
expect_result "an image without linux/amd64 fails" 1 "has no linux/amd64 image" image_case 4.0.0 tagged "linux/arm64"

echo
if [ "$failures" -gt 0 ]; then
    echo "$failures test(s) failed"
    exit 1
fi
echo "All release verification tests passed"
