.PHONY: help build release install clean test lint fmt check docker-setup docker-build docker-run cross-linux cross-mac dev-sync dev-sync-force dev-sync-watch dev-sync-check dev-sync-template

SHELL := /bin/bash
VERSION := $(shell git describe --tags --always --dirty 2>/dev/null || echo "dev")
BINARIES := openflows openflows-setup openflows-dashboard openflows-doctor

help:
	@echo "OpenFlows Build System"
	@echo "======================"
	@echo ""
	@echo "  make build          Build all binaries (debug)"
	@echo "  make release        Build all binaries (release)"
	@echo "  make install        Install binaries to ~/.local/bin"
	@echo "  make clean          Remove build artifacts"
	@echo "  make test           Run all tests"
	@echo "  make lint           Run clippy + fmt check"
	@echo "  make fmt            Format code"
	@echo "  make check          Full CI check (fmt + clippy + build + test)"
	@echo "  make dev-sync        Rebuild + deploy binary if source changed (one-shot)"
	@echo "  make dev-sync-force  Rebuild + deploy binary unconditionally"
	@echo "  make dev-sync-watch  Watch for source changes, auto-redeploy (foreground)"
	@echo "  make dev-sync-check  Exit 1 if binary is stale (CI gate)"
	@echo "  make dev-sync-template  Rebuild binary + push nexus template to Coder"
	@echo "  make docker-build   Build Docker image"
	@echo "  make docker-run     Run via Docker Compose"
	@echo "  make cross-linux    Cross-compile for Linux (x86_64 + aarch64)"
	@echo "  make cross-mac      Cross-compile for macOS (x86_64 + aarch64)"
	@echo "  make dist           Create release tarballs for all platforms"
	@echo ""

build:
	@echo "Building OpenFlows (debug)..."
	cargo build --workspace

release:
	@echo "Building OpenFlows $(VERSION) (release)..."
	cargo build --release -p openflows

install: release
	@echo "Installing OpenFlows binaries..."
	@INSTALL_DIR="$${AGENTFLOW_INSTALL_DIR:-$$HOME/.local/bin}"; \
	mkdir -p "$$INSTALL_DIR"; \
	for bin in $(BINARIES); do \
		cp "target/release/$$bin" "$$INSTALL_DIR/"; \
		chmod +x "$$INSTALL_DIR/$$bin"; \
		echo "  Installed $$INSTALL_DIR/$$bin"; \
	done
	@if [ -d "orchestration" ]; then \
		INSTALL_DIR="$${AGENTFLOW_INSTALL_DIR:-$$HOME/.local/bin}"; \
		cp -r orchestration "$$INSTALL_DIR/"; \
		echo "  Installed orchestration config to $$INSTALL_DIR/orchestration/"; \
	fi
	@echo ""
	@echo "Make sure $${AGENTFLOW_INSTALL_DIR:-$$HOME/.local/bin} is in your PATH."

clean:
	cargo clean
	rm -rf dist/

test:
	cargo nextest run --workspace --all-targets --all-features || \
	cargo test --workspace --all-targets --all-features

lint:
	cargo fmt --all -- --check
	cargo clippy --workspace --all-targets --all-features -- -D warnings

fmt:
	cargo fmt --all

check: fmt lint test
	@echo "All checks passed!"

docker-build:
	docker build -t openflows:$(VERSION) .

# ── Dev binary sync ────────────────────────────────────────────────────
# Keeps the .dev-binaries/openflows binary in sync with source changes.
# Rebuilds, copies to .dev-binaries/, and hot-deploys into the running
# nexus container so the controller picks up new code immediately.

dev-sync:
	@scripts/dev-sync.sh

dev-sync-force:
	@scripts/dev-sync.sh --force

dev-sync-watch:
	@scripts/dev-sync.sh --watch

dev-sync-check:
	@scripts/dev-sync.sh --check

dev-sync-template:
	@scripts/dev-sync.sh --force --push-template

docker-run:
	docker compose up -d

cross-linux:
	@echo "Cross-compiling for Linux..."
	rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-gnu
	cargo build --release --target x86_64-unknown-linux-musl --bin openflows --bin openflows-setup --bin openflows-dashboard --bin openflows-doctor
	cargo build --release --target aarch64-unknown-linux-gnu --bin openflows --bin openflows-setup --bin openflows-dashboard --bin openflows-doctor
	@echo "Linux binaries built:"
	@ls -lh target/x86_64-unknown-linux-musl/release/openflows*
	@ls -lh target/aarch64-unknown-linux-gnu/release/openflows*

cross-mac:
	@echo "Cross-compiling for macOS..."
	rustup target add x86_64-apple-darwin aarch64-apple-darwin
	cargo build --release --target x86_64-apple-darwin --bin openflows --bin openflows-setup --bin openflows-dashboard --bin openflows-doctor
	cargo build --release --target aarch64-apple-darwin --bin openflows --bin openflows-setup --bin openflows-dashboard --bin openflows-doctor
	@echo "macOS binaries built:"
	@ls -lh target/x86_64-apple-darwin/release/openflows*
	@ls -lh target/aarch64-apple-darwin/release/openflows*

dist: release
	@echo "Creating distribution tarballs..."
	@mkdir -p dist
	@PLATFORM="$$(uname -s | tr '[:upper:]' '[:lower:]')"; \
	ARCH="$$(uname -m)"; \
	ARCHIVE="openflows-$(VERSION)-$${PLATFORM}-$${ARCH}"; \
	mkdir -p "dist/$$ARCHIVE"; \
	for bin in $(BINARIES); do \
		cp "target/release/$$bin" "dist/$$ARCHIVE/"; \
	done; \
	cp -r orchestration "dist/$$ARCHIVE/"; \
	cp README.md LICENSE "dist/$$ARCHIVE/" 2>/dev/null || true; \
	tar -czf "dist/$$ARCHIVE.tar.gz" -C dist "$$ARCHIVE"; \
	rm -rf "dist/$$ARCHIVE"; \
	echo "Created dist/$$ARCHIVE.tar.gz"
