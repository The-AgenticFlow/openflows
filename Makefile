.PHONY: help build release install clean test lint fmt check docker-setup docker-build docker-run cross-linux cross-mac

SHELL := /bin/bash
VERSION := $(shell git describe --tags --always --dirty 2>/dev/null || echo "dev")
BINARIES := agentflow agentflow-setup agentflow-dashboard agentflow-doctor

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
	@echo ""
	@echo "Make sure $$INSTALL_DIR is in your PATH."

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

docker-run:
	docker compose up -d

cross-linux:
	@echo "Cross-compiling for Linux..."
	rustup target add x86_64-unknown-linux-musl aarch64-unknown-linux-gnu
	cargo build --release --target x86_64-unknown-linux-musl --bin agentflow --bin agentflow-setup --bin agentflow-dashboard --bin agentflow-doctor
	cargo build --release --target aarch64-unknown-linux-gnu --bin agentflow --bin agentflow-setup --bin agentflow-dashboard --bin agentflow-doctor
	@echo "Linux binaries built:"
	@ls -lh target/x86_64-unknown-linux-musl/release/agentflow*
	@ls -lh target/aarch64-unknown-linux-gnu/release/agentflow*

cross-mac:
	@echo "Cross-compiling for macOS..."
	rustup target add x86_64-apple-darwin aarch64-apple-darwin
	cargo build --release --target x86_64-apple-darwin --bin agentflow --bin agentflow-setup --bin agentflow-dashboard --bin agentflow-doctor
	cargo build --release --target aarch64-apple-darwin --bin agentflow --bin agentflow-setup --bin agentflow-dashboard --bin agentflow-doctor
	@echo "macOS binaries built:"
	@ls -lh target/x86_64-apple-darwin/release/agentflow*
	@ls -lh target/aarch64-apple-darwin/release/agentflow*

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
