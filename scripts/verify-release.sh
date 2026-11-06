#!/usr/bin/env bash
# Checks the files of an OpenFlows release before it is published (#526). Published
# releases are immutable, so a broken file found afterwards can't be replaced.
#
#   verify-release.sh archive FILE.tar.gz VERSION TARGET [--run-with native|rosetta|alpine]
#       Checksum, layout, CPU and C library of the programs; runs each one on this machine.
#   verify-release.sh kit FILE.tar.gz VERSION
#       Checksum, the files `setup.sh --binary` reads, their syntax and the kit's version.
#   verify-release.sh image REGISTRY/OWNER/NAME VERSION TAG_TREE
#       Every platform of the image carries the version label and was built from the
#       tag's source tree. Reads the public registry API; no Docker needed.
#
# Each failure prints `FAIL <artifact>: <reason>`; the exit status is 1 if any check failed.
# Runs on Linux and macOS (bash 3.2).

set -uo pipefail

ALPINE_IMAGE="alpine:3.22@sha256:bfe7ceb9f09ec0566c48fb93356e45a47a00be5a1dd14fa5b909e7709c66b034"
PROGRAMS="openflows openflows-doctor openflows-harness"
# setup.sh --binary builds against glibc 2.17 so the programs run on older Linux hosts.
MAX_GLIBC=2.17
GITHUB_REPO="${GITHUB_REPOSITORY:-The-AgenticFlow/openflows}"

failures=0
SUBJECT=""
fail() { printf 'FAIL %s: %s\n' "$SUBJECT" "$1"; failures=$((failures + 1)); }
pass() { printf 'ok   %s: %s\n' "$SUBJECT" "$1"; }
sha256_of() { if command -v sha256sum >/dev/null 2>&1; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'; }

# check_checksum FILE: FILE.sha256 names FILE and matches its contents.
check_checksum() {
    local file="$1" sums="${1}.sha256" want name
    if [ ! -s "$sums" ]; then fail "missing $(basename "$sums")"; return 1; fi
    want="$(awk '{print $1}' "$sums")"
    name="$(awk '{print $2}' "$sums")"
    name="${name#\*}"
    if [ "$name" != "$(basename "$file")" ]; then
        fail "checksum file names '${name}', not $(basename "$file")"
    fi
    if [ "$want" != "$(sha256_of "$file")" ]; then fail "checksum does not match"; return 1; fi
    pass "checksum verified"
}

# expected_file_type TARGET: the text `file -b` prints for a program built for TARGET.
expected_file_type() {
    case "$1" in
        x86_64-unknown-linux-gnu)  echo 'ELF 64-bit LSB.*x86-64.*dynamically linked' ;;
        x86_64-unknown-linux-musl) echo 'ELF 64-bit LSB.*x86-64.*static' ;;
        aarch64-unknown-linux-gnu) echo 'ELF 64-bit LSB.*ARM aarch64.*dynamically linked' ;;
        x86_64-apple-darwin)       echo 'Mach-O 64-bit.*x86_64' ;;
        aarch64-apple-darwin)      echo 'Mach-O 64-bit.*arm64' ;;
    esac
}

# newest_glibc PROGRAM: the highest GLIBC_x.y symbol version the program needs.
newest_glibc() {
    objdump -T "$1" 2>/dev/null | grep -o 'GLIBC_[0-9][0-9.]*' | sed 's/GLIBC_//' | sort -V | tail -1
}

# run_program HOW DIR PROGRAM ARGS...: run a program from DIR natively, under Rosetta or in Alpine.
run_program() {
    local how="$1" dir="$2" program="$3"
    shift 3
    case "$how" in
        native)  "${dir}/${program}" "$@" ;;
        rosetta) arch -x86_64 "${dir}/${program}" "$@" ;;
        alpine)  docker run --rm --network none -v "${dir}:/release:ro" "$ALPINE_IMAGE" "/release/${program}" "$@" ;;
        *) echo "unknown --run-with '${how}'" >&2; return 2 ;;
    esac
}

verify_archive() {
    local file="$1" version="$2" target="$3" how="${4:-native}" expected work dir program type glibc out
    SUBJECT="$(basename "$file")"
    if [ "$SUBJECT" != "openflows-${version}-${target}.tar.gz" ]; then
        fail "expected the name openflows-${version}-${target}.tar.gz"
    fi
    expected="$(expected_file_type "$target")"
    if [ -z "$expected" ]; then fail "unknown target ${target}"; return; fi
    if [ ! -s "$file" ]; then fail "missing or empty"; return; fi
    check_checksum "$file" || return

    work="$(mktemp -d)"
    if ! tar -xzf "$file" -C "$work" 2>/dev/null; then fail "is not a readable tar.gz"; rm -rf -- "$work"; return; fi
    dir="${work}/openflows-${version}-${target}"
    if [ ! -d "$dir" ]; then fail "has no openflows-${version}-${target}/ folder"; rm -rf -- "$work"; return; fi

    for program in $PROGRAMS; do
        if [ ! -f "${dir}/${program}" ] || [ ! -x "${dir}/${program}" ]; then
            fail "${program} is missing or not executable"; continue
        fi
        type="$(file -b "${dir}/${program}")"
        if printf '%s' "$type" | grep -Eq "$expected"; then pass "${program} is built for ${target}"
        else fail "${program} is not built for ${target} (${type})"; fi
        case "$target" in
            *-linux-gnu)
                glibc="$(newest_glibc "${dir}/${program}")"
                if [ -z "$glibc" ]; then fail "${program}: could not read its glibc symbols"
                elif [ "$(printf '%s\n' "$MAX_GLIBC" "$glibc" | sort -V | tail -1)" != "$MAX_GLIBC" ]; then
                    fail "${program} needs glibc ${glibc}, newer than ${MAX_GLIBC}"
                else pass "${program} needs glibc ${glibc} at most"; fi ;;
        esac
    done

    if [ "$how" = alpine ] && ! docker pull -q "$ALPINE_IMAGE" >/dev/null 2>&1; then
        fail "could not pull ${ALPINE_IMAGE}"; rm -rf -- "$work"; return
    fi
    out="$(run_program "$how" "$dir" openflows --version 2>&1)"
    if [ "$out" = "openflows ${version}" ]; then pass "openflows --version prints ${version} (${how})"
    else fail "openflows --version printed '${out}', not 'openflows ${version}' (${how})"; fi
    for program in openflows-doctor openflows-harness; do
        if run_program "$how" "$dir" "$program" --help >/dev/null 2>&1; then pass "${program} runs (${how})"
        else fail "${program} --help failed (${how})"; fi
    done
    rm -rf -- "$work"
}

# Files setup.sh reads from a setup kit, besides each Compose file it names.
KIT_FILES="RELEASE_VERSION .env.example scripts/setup.sh scripts/setup-release.sh scripts/prod.sh scripts/connected-local-github.py"

verify_kit() {
    local file="$1" version="$2" work dir path kit_version compose ref
    SUBJECT="$(basename "$file")"
    if [ "$SUBJECT" != "openflows-${version}-setup.tar.gz" ]; then
        fail "expected the name openflows-${version}-setup.tar.gz"
    fi
    if [ ! -s "$file" ]; then fail "missing or empty"; return; fi
    check_checksum "$file" || return
    work="$(mktemp -d)"
    if ! tar -xzf "$file" -C "$work" 2>/dev/null; then fail "is not a readable tar.gz"; rm -rf -- "$work"; return; fi
    dir="${work}/openflows-${version}"
    if [ ! -d "$dir" ]; then fail "has no openflows-${version}/ folder"; rm -rf -- "$work"; return; fi

    for path in $KIT_FILES; do
        [ -f "${dir}/${path}" ] || fail "missing ${path}"
    done
    kit_version="$(cat "${dir}/RELEASE_VERSION" 2>/dev/null)"
    if [ "$kit_version" = "$version" ]; then pass "RELEASE_VERSION is ${version}"
    else fail "RELEASE_VERSION is '${kit_version}', not ${version}"; fi

    # Every Compose file setup.sh can select, and every ./deploy path those files mount.
    # docker-compose.override.yml is the user's own optional file, not part of a release.
    while read -r compose; do
        [ -n "$compose" ] || continue
        if [ ! -f "${dir}/${compose}" ]; then fail "missing ${compose}, which setup.sh uses"; continue; fi
        while read -r ref; do
            [ -z "$ref" ] || [ -e "${dir}/${ref#./}" ] || fail "missing ${ref#./}, which ${compose} mounts"
        done < <(grep -Eo '\./deploy/[A-Za-z0-9._/-]+' "${dir}/${compose}" | sort -u)
    done < <(grep -Eho 'docker-compose[A-Za-z0-9.-]*\.yml' "${dir}/scripts/setup.sh" 2>/dev/null | grep -v '^docker-compose\.override\.' | sort -u)
    for path in scripts/setup.sh scripts/setup-release.sh scripts/prod.sh; do
        [ -f "${dir}/${path}" ] || continue
        bash -n "${dir}/${path}" 2>/dev/null || fail "${path} has a syntax error"
    done
    if [ -f "${dir}/scripts/connected-local-github.py" ] && command -v python3 >/dev/null 2>&1; then
        python3 -c 'import ast,sys; ast.parse(open(sys.argv[1]).read())' "${dir}/scripts/connected-local-github.py" 2>/dev/null \
            || fail "scripts/connected-local-github.py has a syntax error"
    fi
    pass "contents checked"
    rm -rf -- "$work"
}

# registry_get REGISTRY PATH TOKEN: GET from the registry API, accepting indexes and manifests.
registry_get() {
    curl -fsSL -m 60 -H "Authorization: Bearer ${3}" \
        -H 'Accept: application/vnd.oci.image.index.v1+json, application/vnd.oci.image.manifest.v1+json, application/vnd.docker.distribution.manifest.list.v2+json, application/vnd.docker.distribution.manifest.v2+json' \
        "https://${1}/v2/${2}"
}
registry_token() { # registry_token REGISTRY NAME: an anonymous pull token
    curl -fsS -m 30 "https://${1}/token?scope=repository:${2}:pull" | jq -r .token
}
# tree_of COMMIT: the commit's source tree, from this checkout or from GitHub.
tree_of() {
    git rev-parse --verify -q "${1}^{tree}" 2>/dev/null \
        || gh api "repos/${GITHUB_REPO}/commits/${1}" --jq .commit.tree.sha 2>/dev/null
}

verify_image() {
    local image="$1" version="$2" tag_tree="$3" registry name token top digests digest manifest config labels label revision platform platforms
    registry="${image%%/*}"
    name="${image#*/}"
    SUBJECT="${image}:${version}"
    if ! token="$(registry_token "$registry" "$name")" || [ -z "$token" ]; then fail "could not get a registry token"; return; fi
    if ! top="$(registry_get "$registry" "${name}/manifests/${version}" "$token")"; then fail "not found in the registry"; return; fi

    # An index lists one manifest per platform plus attestations (platform unknown/unknown).
    if [ "$(printf '%s' "$top" | jq -r 'has("manifests")')" = true ]; then
        digests="$(printf '%s' "$top" | jq -r '.manifests[] | select(.platform.os != "unknown") | .digest')"
    else
        digests="$version"
    fi
    if [ -z "$digests" ]; then fail "lists no platforms"; return; fi
    platforms=""
    for digest in $digests; do
        if ! manifest="$(registry_get "$registry" "${name}/manifests/${digest}" "$token")"; then fail "cannot read manifest ${digest}"; continue; fi
        config="$(printf '%s' "$manifest" | jq -r '.config.digest // empty')"
        if [ -z "$config" ] || ! labels="$(registry_get "$registry" "${name}/blobs/${config}" "$token")"; then
            fail "cannot read the configuration of ${digest}"; continue
        fi
        platform="$(printf '%s' "$labels" | jq -r '"\(.os)/\(.architecture)"')"
        platforms="${platforms} ${platform}"
        label="$(printf '%s' "$labels" | jq -r '.config.Labels["org.opencontainers.image.version"] // empty')"
        revision="$(printf '%s' "$labels" | jq -r '.config.Labels["org.opencontainers.image.revision"] // empty')"
        if [ "$label" = "$version" ]; then pass "${platform} is labelled ${version}"
        else fail "${platform} is labelled '${label}', not ${version}"; fi
        # The label can name the commit that started the build rather than the tag; the
        # source is what matters, so compare trees.
        if [ -z "$revision" ]; then fail "${platform} has no revision label"
        elif [ "$(tree_of "$revision")" = "$tag_tree" ]; then pass "${platform} was built from the tag's source (${revision:0:12})"
        else fail "${platform} was built from ${revision:0:12}, whose source differs from the tag"; fi
    done
    case " ${platforms} " in
        *" linux/amd64 "*) ;;
        *) fail "has no linux/amd64 image, which setup.sh pulls" ;;
    esac
}

main() {
    local cmd="${1:-}"
    shift || true
    case "$cmd" in
        archive)
            [ $# -ge 3 ] || { echo "usage: $0 archive FILE VERSION TARGET [--run-with native|rosetta|alpine]" >&2; return 2; }
            local how=native
            if [ "${4:-}" = --run-with ]; then how="${5:-}"; fi
            verify_archive "$1" "$2" "$3" "$how" ;;
        kit)
            [ $# -eq 2 ] || { echo "usage: $0 kit FILE VERSION" >&2; return 2; }
            verify_kit "$1" "$2" ;;
        image)
            [ $# -eq 3 ] || { echo "usage: $0 image REGISTRY/OWNER/NAME VERSION TAG_TREE" >&2; return 2; }
            verify_image "$1" "$2" "$3" ;;
        *) sed -n '2,15p' "$0" >&2; return 2 ;;
    esac
    if [ "$failures" -gt 0 ]; then
        printf '%s check(s) failed\n' "$failures"
        return 1
    fi
}

if [ "${BASH_SOURCE[0]}" = "$0" ]; then
    main "$@"
fi
