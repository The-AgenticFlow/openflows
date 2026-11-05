#!/usr/bin/env bash
# Sourced by setup.sh: gets OpenFlows from a published image (--compose) or release files (--binary).

# ── stage 2b: published image (--compose) ───────────────────────────────────
# host_target — the release target for this machine; empty when there is none.
host_target() {
    case "$(uname -s)-$(uname -m)" in
        Linux-x86_64|Linux-amd64)
            if getconf GNU_LIBC_VERSION >/dev/null 2>&1; then echo x86_64-unknown-linux-gnu
            else echo x86_64-unknown-linux-musl; fi ;;
        Linux-aarch64|Linux-arm64) echo aarch64-unknown-linux-gnu ;;
        Darwin-x86_64) echo x86_64-apple-darwin ;;
        Darwin-arm64|Darwin-aarch64) echo aarch64-apple-darwin ;;
    esac
}
# needs_host_programs — true when the workspaces' linux/amd64 programs can't run on this machine.
# The image's are built against glibc 2.34; the release's against 2.17.
needs_host_programs() {
    [ "$(host_target)" = "$WORKSPACE_TARGET" ] || return 0
    [ "$MODE" = compose ] || return 1
    local glibc
    glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null | sed -n 's/^glibc //p')"
    [ "$(printf '%s\n' 2.34 "$glibc" | sort -V | head -1)" != 2.34 ]
}
sha256_of() { if have sha256sum; then sha256sum "$1"; else shasum -a 256 "$1"; fi | awk '{print $1}'; }
# install_archive VERSION TARGET DEST — download a release archive, verify its checksum and
# install its programs. Returns 1 when the release has no such file, 2 on a checksum mismatch.
install_archive() {
    local version="$1" target="$2" dest="$3" tmp name bin rc=0
    name="openflows-${version}-${target}"
    tmp="$(mktemp -d)"
    (
        cd "$tmp"
        local base="https://github.com/${RELEASE_REPO}/releases/download/openflows-${version}"
        curl -fsSL -m 900 -o "${name}.tar.gz" "${base}/${name}.tar.gz" || exit 1
        curl -fsSL -m 60 -o "${name}.sha256" "${base}/${name}.tar.gz.sha256" || exit 1
        [ "$(awk '{print $1}' "${name}.sha256")" = "$(sha256_of "${name}.tar.gz")" ] || exit 2
        tar -xzf "${name}.tar.gz" || exit 2
        mkdir -p "$dest"
        for bin in openflows openflows-doctor openflows-harness; do
            cp "${name}/${bin}" "${dest}/${bin}.tmp" && chmod 0755 "${dest}/${bin}.tmp" \
                && mv "${dest}/${bin}.tmp" "${dest}/${bin}" || exit 2
        done
    ) || rc=$?
    rm -r -- "$tmp"
    return "$rc"
}
fetch_programs() { # fetch_programs VERSION TARGET DEST — install_archive, or stop with what to do
    local rc=0
    install_archive "$1" "$2" "$3" >>"$LOG_FILE" 2>&1 &
    spin $! "Downloading the ${2} programs" || rc=$?
    case "$rc" in
        0) ;;
        2) die "The ${2} archive of OpenFlows ${1} failed verification." "Run setup again; if it fails again, do not use this download." ;;
        *) die "OpenFlows ${1} has no ${2} programs to download." \
               "Releases 1.8.1 and older have no attached files. Set a newer OPENFLOWS_VERSION in .env," \
               "or build from source:  ./scripts/setup.sh ${REPO} --source" ;;
    esac
}
# programs_intact DIR — the installed programs are the ones the last download verified. Catches
# a deleted, damaged or non-executable program and a switch that was interrupted halfway.
programs_intact() {
    local bin
    for bin in openflows openflows-doctor openflows-harness; do
        [ -f "${1}/${bin}" ] && [ -x "${1}/${bin}" ] || return 1
        [ "$(sha256_of "${1}/${bin}")" = "$(awk -v b="$bin" '$2 == b {print $1}' "${1}/CHECKSUMS" 2>/dev/null)" ] || return 1
    done
}
record_checksums() { # record_checksums DIR
    local bin
    for bin in openflows openflows-doctor openflows-harness; do
        printf '%s  %s\n' "$(sha256_of "${1}/${bin}")" "$bin"
    done >"${1}/CHECKSUMS"
}
# use_host_programs VERSION — make sure setup can run OpenFlows on this machine: `OPENFLOWS_BIN` is the
# workspaces' copy if it runs here, else a native copy from the release in .dev-binaries/host.
use_host_programs() {
    local version="$1"
    export OPENFLOWS_BIN="${BIN_DIR}/openflows"
    if ! needs_host_programs; then
        [ ! -d "$HOST_BIN_DIR" ] || rm -r -- "$HOST_BIN_DIR"
        return 0
    fi
    if [ "$(cat "${HOST_BIN_DIR}/VERSION" 2>/dev/null)" != "$version" ] || ! programs_intact "$HOST_BIN_DIR"; then
        fetch_programs "$version" "$(host_target)" "$HOST_BIN_DIR"
        printf '%s\n' "$version" >"${HOST_BIN_DIR}/VERSION"
        record_checksums "$HOST_BIN_DIR"
    fi
    export OPENFLOWS_BIN="${HOST_BIN_DIR}/openflows"
    "$OPENFLOWS_BIN" --version >/dev/null 2>&1 \
        || die "The $(host_target) OpenFlows program does not run on this machine." \
            "Build from source instead:  ./scripts/setup.sh ${REPO} --source"
}
# resolve_version — OPENFLOWS_VERSION, else this setup kit's version, else the latest release.
resolve_version() {
    local version
    version="$(env_get OPENFLOWS_VERSION)"
    [ -n "$version" ] || version="$(cat "${ROOT}/RELEASE_VERSION" 2>/dev/null || true)"
    if [ -z "$version" ]; then
        version="$(latest_release_version)"
        [ -n "$version" ] || die "Could not look up the latest OpenFlows release." \
            "Check your connection, or set OPENFLOWS_VERSION in .env (e.g. 1.8.1)."
    fi
    [[ "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+$ ]] || die "OPENFLOWS_VERSION must look like 1.8.1 (got '${version}')."
    printf '%s' "$version"
}
installed_ref() { # the exact programs this install runs; a change means pushing templates again
    if [ "$MODE" = compose ]; then image_ref; else printf 'release:%s' "$(env_get OPENFLOWS_VERSION)"; fi
}
stage_release() {
    step "OpenFlows release (no compiling)"
    local version
    version="$(resolve_version)"
    mkdir -p "$BIN_DIR"
    # Host programs first: if their download fails, the workspaces' programs stay untouched.
    use_host_programs "$version"
    if [ "$(cat "${BIN_DIR}/RELEASE" 2>/dev/null)" != "$version" ] || [ -f "${BIN_DIR}/IMAGE" ] || ! programs_intact "$BIN_DIR"; then
        fetch_programs "$version" "$WORKSPACE_TARGET" "$BIN_DIR"
        printf '%s\n' "$version" >"${BIN_DIR}/RELEASE"
        record_checksums "$BIN_DIR"
        rm -f "${BIN_DIR}/IMAGE"
    fi
    "$OPENFLOWS_BIN" --version >/dev/null 2>&1 \
        || die "The OpenFlows programs do not run on this machine." \
            "Build from source instead:  ./scripts/setup.sh ${REPO} --source"
    env_set OPENFLOWS_VERSION "$version"
    env_set OPENFLOWS_IMAGE_DIGEST ""
    ok "OpenFlows ${B}${version}${N} · checksums verified"
}
image_ref() { # image_ref → the exact image the stack runs, e.g. ghcr.io/…/openflows:1.8.1@sha256:…
    printf '%s:%s@%s' "$(image_repo)" "$(env_get OPENFLOWS_VERSION)" "$(env_get OPENFLOWS_IMAGE_DIGEST)"
}

latest_release_version() { # latest published release, e.g. 1.8.1
    curl -fsS -m 20 "https://api.github.com/repos/${RELEASE_REPO}/releases/latest" 2>/dev/null \
        | json tag_name | sed -n 's/^openflows-\([0-9][0-9.]*\)$/\1/p'
}

stage_image() {
    step "OpenFlows image (no compiling)"
    local version digest pinned label repo
    version="$(resolve_version)"
    repo="$(image_repo)"

    pinned="$(env_get OPENFLOWS_IMAGE_DIGEST)"
    if [ -n "$pinned" ]; then
        docker pull -q --platform linux/amd64 "${repo}@${pinned}" >"$LOG_FILE" 2>&1 \
            || die "Image ${repo}@${pinned} was not found." \
                "Check OPENFLOWS_IMAGE_DIGEST in .env against the release notes, or clear it to use ${version}'s current image."
        digest="$pinned"
    else
        docker pull -q --platform linux/amd64 "${repo}:${version}" >"$LOG_FILE" 2>&1 \
            || die "No published image for OpenFlows ${version}." \
                "Pick a version that has one: https://github.com/The-AgenticFlow/openflows/pkgs/container/openflows"
        digest="$(docker image inspect --format '{{range .RepoDigests}}{{println .}}{{end}}' "${repo}:${version}" \
            | sed -n "s|^${repo}@||p" | head -1)"
        [ -n "$digest" ] || die "Could not read the digest of ${repo}:${version}."
    fi
    # Docker ignores the tag when a digest is given, so a digest left over from
    # another version would otherwise install that version silently.
    label="$(docker image inspect --format '{{index .Config.Labels "org.opencontainers.image.version"}}' "${repo}@${digest}" 2>/dev/null || true)"
    if [ -n "$label" ] && [ "$label" != "$version" ]; then
        die "OPENFLOWS_IMAGE_DIGEST is for version ${label}, not ${version}." \
            "Clear OPENFLOWS_IMAGE_DIGEST in .env, or set OPENFLOWS_VERSION=${label}."
    fi

    env_set OPENFLOWS_VERSION "$version"
    env_set OPENFLOWS_IMAGE_DIGEST "$digest"
    env_set OPENFLOWS_HOST_UID "$(id -u)"
    env_set OPENFLOWS_HOST_GID "$(id -g)"
    mkdir -p "$BIN_DIR"   # created by us, not by Docker, so it stays owned by this user
    rm -f "${BIN_DIR}/RELEASE"
    use_host_programs "$version"
    ok "OpenFlows ${B}${version}${N} · ${digest:0:19}…"
}
