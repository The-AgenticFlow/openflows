# ──────────────────────────────────────────────
# Stage 1: Build
# ──────────────────────────────────────────────
FROM rust:1.88-bookworm AS builder

WORKDIR /app

# Install musl tools for static linking (optional, for smaller images)
RUN apt-get update && apt-get install -y --no-install-recommends \
    musl-tools \
    && rm -rf /var/lib/apt/lists/*

# Copy workspace manifests first for layer caching
COPY Cargo.toml Cargo.lock ./
COPY crates ./crates
COPY binary ./binary

# Build release binaries
RUN cargo build --release -p openflows

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
COPY --from=builder /app/target/release/agentflow /usr/local/bin/
COPY --from=builder /app/target/release/agentflow-setup /usr/local/bin/
COPY --from=builder /app/target/release/agentflow-dashboard /usr/local/bin/
COPY --from=builder /app/target/release/agentflow-doctor /usr/local/bin/

# Set permissions
RUN chmod +x /usr/local/bin/agentflow*

# Create workspace directory
RUN mkdir -p /workspace && chown -R openflows:openflows /workspace

# Switch to non-root user
USER openflows
WORKDIR /workspace

# Health check
HEALTHCHECK --interval=30s --timeout=10s --start-period=5s --retries=3 \
    CMD pgrep -x agentflow > /dev/null || exit 1

ENTRYPOINT ["agentflow"]
CMD ["--help"]
