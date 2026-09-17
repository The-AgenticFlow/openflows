# ──────────────────────────────────────────────
# Stage 1: Build
# ──────────────────────────────────────────────
FROM rust:1.88-bookworm AS builder

WORKDIR /app

# Install musl tools for static linking (optional, for smaller images)
RUN apt-get update && apt-get install -y --no-install-recommends \
    musl-tools \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace manifests first so `cargo fetch` can resolve and cache the
# dependency tree before the rest of the build context changes. This keeps the
# crate-download layer stable — combined with the buildx `type=gha` cache in
# docker-publish.yml, unchanged dependencies are not re-fetched on re-runs.
COPY Cargo.toml Cargo.lock ./

# Download (and cache in the layer cache) all crates.io dependencies ahead of
# copying source, so network resolution isn't repeated for every build.
RUN cargo fetch

COPY crates ./crates
COPY binary ./binary
COPY orchestration ./orchestration

# Build all release binaries
RUN cargo build --release --bin openflows --bin openflows-doctor --bin openflows-harness

# ──────────────────────────────────────────────
# Stage 2: Runtime
# ──────────────────────────────────────────────
FROM debian:bookworm-slim AS runtime

# Install runtime dependencies
RUN apt-get update && apt-get install -y --no-install-recommends \
    ca-certificates \
    curl \
    git \
    nodejs \
    npm \
    && rm -rf /var/lib/apt/lists/*

# Install Claude Code CLI globally
RUN npm install -g @anthropic-ai/claude-code

# Create non-root user
RUN groupadd -r openflows && useradd -r -g openflows -m -d /home/openflows openflows

# Copy binaries from builder
COPY --from=builder /app/target/release/openflows         /usr/local/bin/
COPY --from=builder /app/target/release/openflows-doctor  /usr/local/bin/
COPY --from=builder /app/target/release/openflows-harness /usr/local/bin/

# Set permissions
RUN chmod +x /usr/local/bin/openflows*

# Create workspace directory
RUN mkdir -p /workspace && chown -R openflows:openflows /workspace

# Switch to non-root user
USER openflows
WORKDIR /workspace

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD pgrep -x openflows > /dev/null || exit 1

ENTRYPOINT ["openflows"]
CMD ["--help"]
