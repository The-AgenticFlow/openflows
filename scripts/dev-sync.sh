#!/bin/bash
# Sync dev binaries to .dev-binaries/ for Coder workspace mounting
#
# This script:
# 1. Builds the openflows binary (if needed)
# 2. Copies it to .dev-binaries/ for Docker mounting
# 3. Optionally hot-deploys into running Nexus workspace

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROJECT_ROOT="$(cd "${SCRIPT_DIR}/.." && pwd)"
DEV_BINARIES_DIR="${PROJECT_ROOT}/.dev-binaries"
# The host CLI is native (macOS or Linux); these artifacts execute inside
# the amd64 Linux Coder workspace, even on Intel/Apple Silicon Macs.
LINUX_TARGET="x86_64-unknown-linux-musl"
RELEASE_BIN="${PROJECT_ROOT}/target/${LINUX_TARGET}/release/openflows"
HARNESS_BIN="${PROJECT_ROOT}/target/${LINUX_TARGET}/release/openflows-harness"

echo "═══════════════════════════════════════"
echo "  OpenFlows Dev Binary Sync"
echo "═══════════════════════════════════════"
echo ""

# Step 1: Always rebuild from current source. `cargo build` is incremental,
# so this is a fast no-op when nothing changed, but it guarantees we never
# sync a stale binary into .dev-binaries/ (and from there into workspaces)
# just because a binary happened to already exist on disk from a previous
# build predating a recent code fix.
echo "Step 1: Building openflows binaries for ${LINUX_TARGET} (release mode)..."

cd "$PROJECT_ROOT"

# Some Rust crates (e.g. `ring`) bundle C/asm that needs a C cross-compiler
# matching the target. macOS has no `x86_64-linux-musl-gcc` out of the box,
# so plain `cargo build --target x86_64-unknown-linux-musl` fails with
# "failed to find tool x86_64-linux-musl-gcc". Try, in order:
#   1. `cargo zigbuild`           — zig as the cross-CC; portable, no per-target toolchain.
#   2. musl-cross gcc on PATH     — e.g. `brew install messense/musl-cross/musl-cross-x86_64`.
#   3. Docker                     — `messense/rust-musl-cross` is purpose-built for this target.
# Otherwise fail with explicit install instructions.
DOCKER_CARGO_HOME="${PROJECT_ROOT}/.docker-cargo"
BUILD_CMD=()
HOST_TOOLCHAIN=false
export CARGO_TARGET_DIR="${PROJECT_ROOT}/target"
if command -v cargo-zigbuild >/dev/null 2>&1 && command -v zig >/dev/null 2>&1; then
    echo "  → Using cargo-zigbuild (zig cross-CC)"
    BUILD_CMD=(cargo zigbuild)
    HOST_TOOLCHAIN=true
elif command -v "x86_64-linux-musl-gcc" >/dev/null 2>&1; then
    echo "  → Using host gcc x86_64-linux-musl-gcc"
    BUILD_CMD=(cargo build)
    HOST_TOOLCHAIN=true
    export CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-linux-musl-gcc
elif command -v docker >/dev/null 2>&1 && docker info >/dev/null 2>&1; then
    # Messense's image is the de-facto standard for cross-compiling Rust to
    # x86_64-unknown-linux-musl. It ships the matching gcc and rust stdlib
    # so `cargo build --target x86_64-unknown-linux-musl` just works.
    # We mount the source and a throwaway CARGO_HOME to avoid polluting the
    # host's cargo cache with linux-musl artifacts.
    echo "  → Using Docker (messense/rust-musl-cross:x86_64-musl)"
    BUILD_CMD=(docker run --rm
        -v "${PROJECT_ROOT}:/src"
        -w /src
        -e CARGO_HOME=/src/.docker-cargo
        -e CARGO_TARGET_DIR=/src/target
        -e CARGO_TARGET_X86_64_UNKNOWN_LINUX_MUSL_LINKER=x86_64-unknown-linux-musl-gcc
        "messense/rust-musl-cross:x86_64-musl" cargo build)
    mkdir -p "${DOCKER_CARGO_HOME}"
else
    echo "❌ No cross C toolchain found for ${LINUX_TARGET}." >&2
    echo "   Install one of:" >&2
    echo "     brew install zig && cargo install cargo-zigbuild --locked" >&2
    echo "     brew install messense/musl-cross/musl-cross-x86_64" >&2
    echo "   Or ensure Docker is running so we can build in a Linux container." >&2
    exit 1
fi
# Docker carries its own Rust target and C compiler; only local builds need
# the target installed on the host. Docker selects a native builder image on
# Intel/Apple Silicon; the Rust output target remains Linux amd64.
if [ "$HOST_TOOLCHAIN" = true ]; then
    if ! command -v rustup >/dev/null 2>&1; then
        echo "❌ rustup is required for the selected host cross-toolchain." >&2
        exit 1
    fi
    if ! rustup target list --installed | grep -qx "$LINUX_TARGET"; then
        rustup target add "$LINUX_TARGET"
    fi
fi
"${BUILD_CMD[@]}" --release --target "${LINUX_TARGET}" -p openflows -p openflows-harness
echo "✓ Build complete"
echo "  openflows:        $(du -h "$RELEASE_BIN" | cut -f1)"
echo "  openflows-harness: $(du -h "$HARNESS_BIN" | cut -f1)"
echo ""

# Sanity check: refuse to sync anything that isn't a Linux ELF. A Mach-O or
# other-arch binary copied into /opt/openflows-dev/ ends up overwriting
# /usr/local/bin/openflows inside the Linux workspace and bricks the
# controller with cryptic `__PAGEZERO__: not found` / "Unterminated quoted
# string" errors when the kernel returns ENOEXEC and sh falls back to
# interpreting the binary as a script.
verify_elf() {
    local bin="$1"
    local ident machine
    ident=$(od -An -tx1 -N6 "$bin" | tr -d ' \n')
    machine=$(od -An -tx1 -j18 -N2 "$bin" | tr -d ' \n')
    if [ "$ident" != "7f454c460201" ] || [ "$machine" != "3e00" ]; then
        echo "❌ $bin is not a little-endian x86-64 ELF binary." >&2
        echo "   file: $(file -b "$bin" 2>/dev/null || head -c 16 "$bin" | od -c | head -1)" >&2
        echo "   Refusing to sync — this would brick the Coder workspace." >&2
        exit 1
    fi
}
verify_elf "$RELEASE_BIN"
verify_elf "$HARNESS_BIN"

# Step 2: Sync to .dev-binaries
echo "Step 2: Syncing binaries to .dev-binaries/..."
mkdir -p "$DEV_BINARIES_DIR"
cp -v "$RELEASE_BIN" "$DEV_BINARIES_DIR/openflows"
chmod +x "$DEV_BINARIES_DIR/openflows"
cp -v "$HARNESS_BIN" "$DEV_BINARIES_DIR/openflows-harness"
chmod +x "$DEV_BINARIES_DIR/openflows-harness"
echo "✓ Binaries synced"
echo "  openflows:        $DEV_BINARIES_DIR/openflows"
echo "  openflows-harness: $DEV_BINARIES_DIR/openflows-harness"
echo ""

# Step 3: Optional hot-deploy to running workspace
if command -v docker >/dev/null 2>&1; then
    NEXUS_CONTAINER=$(docker ps --filter "name=openflows-nexus" --format "{{.Names}}" 2>/dev/null | head -1 || echo "")
    if [ -n "$NEXUS_CONTAINER" ]; then
        echo "Step 3: Hot-deploying to running workspace ($NEXUS_CONTAINER)..."
        docker cp "$DEV_BINARIES_DIR/openflows" "$NEXUS_CONTAINER:/usr/local/bin/openflows"
        docker exec -u 0 "$NEXUS_CONTAINER" chmod +x /usr/local/bin/openflows
        echo "✓ Hot-deployed"
        echo ""
        echo "Tip: Restart the controller in the workspace with:"
        echo "  pkill -f 'openflows run' || true"
        echo "  openflows run"
    else
        echo "Step 3: No running Nexus workspace found (will be used on next workspace start)"
        echo ""
    fi
else
    echo "Step 3: Docker not available; binary will be used on next workspace start"
    echo ""
fi

echo "═══════════════════════════════════════"
echo "✓ Dev binary sync complete"
echo "═══════════════════════════════════════"
