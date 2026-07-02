# OpenFlows Project Guide

OpenFlows is an autonomous AI software development team that runs itself. It orchestrates a team of specialized AI agents (NEXUS, FORGE, SENTINEL, VESSEL, LORE) to transform GitHub issues into pull requests autonomously.

## Build and Toolchain
- **Language**: Rust (1.70+), Bash, Node.js (18+)
- **Build System**: `make` or `cargo`
- **Primary Commands**:
  - `make build`: Build all binaries (debug)
  - `make release`: Build all binaries (release)
  - `make test`: Run all unit and integration tests (uses `cargo nextest` if available)
  - `make lint`: Run `clippy` and `fmt` checks
  - `make fmt`: Format all code
  - `make check`: Full CI-quality check (fmt + lint + build + test)
- **Execution**:
  - `cargo run --bin openflows`: Start the autonomous team
  - `cargo run --bin demo`: Run mocked demo (no API keys required)
  - `cargo run --bin openflows-setup`: Interactive setup wizard

## Architecture Summary
The project is a Rust workspace composed of several specialized crates:
- `pocketflow-core`: Core engine, shared store, and routing.
- `agent-nexus`: The Orchestrator (assigns issues to worker slots).
- `agent-forge`: The Builder (spawns Claude Code/Codex to write code).
- `agent-sentinel`: The Reviewer (verifies Forge's work segments).
- `agent-vessel`: The DevOps (manages CI and merging).
- `agent-lore`: The Writer (handles documentation and logging).
- `pair-harness`: Manages process spawning and Git worktrees for isolation.
- `binary`: CLI entry points for the tools.

### Isolation Model
OpenFlows uses a three-layer isolation model for agents:
1. **Git worktrees**: Each agent pair works in an isolated checkout in `worktrees/pair-N/`.
2. **File ownership**: Shared Redis-based locking prevents multiple agents from editing the same file.
3. **Slot directories**: Artifacts and communication logs are scoped to `orchestration/pairs/pair-N/`.

## Coding Conventions
- **Rust Standard**: Idiomatic Rust (Edition 2021).
- **Error Handling**: Use `anyhow` for top-level errors and `thiserror` for library-level errors.
- **Async**: `tokio` is used throughout for async operations.
- **Formatting**: Always run `make fmt` before committing.
- **Linting**: No `clippy` warnings allowed (enforced by `make lint`).
- **Commit Messages**: Follow conventional commits:
  - `feat:` (new feature)
  - `fix:` (bug fix)
  - `docs:` (documentation)
  - `test:` (adding tests)
  - `refactor:` (code improvement)
  - `chore:` (maintenance)

## Test Requirements
- **Unit Tests**: Place in the `src/` directory of the respective crate.
- **Integration Tests**: Place in the `tests/` directory within each crate.
- **Verification**: Run `make test` to ensure all tests pass across the workspace. New features MUST include corresponding tests.
- **E2E Tests**: Dedicated tests for agent logic (e.g., `nexus_e2e`) are located in `tests/` folders.

## Environment & Backends
- **Required Keys**: `GITHUB_PERSONAL_ACCESS_TOKEN`.
- **AI Backends**: Supported via `DEFAULT_CLI=claude` (Anthropic) or `DEFAULT_CLI=codex` (OpenAI/Fireworks).
- **Configuration**: Managed via `.env` file or `openflows-setup` wizard.
