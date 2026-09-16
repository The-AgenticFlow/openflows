# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [1.3.0](https://github.com/The-AgenticFlow/openflows/compare/openflows-1.2.1...openflows-1.3.0) - 2026-09-16

### Added

- prefer GitHub PAT for workspace git, fix repo acquisition
- *(coder)* pin Coder to v2.37.0 and validate version in doctor ([#186](https://github.com/The-AgenticFlow/openflows/pull/186))
- *(config)* centralize remaining env reads incl GITHUB_API_BASE (#185, #204)
- *(config)* centralize environment configuration with envconfig ([#185](https://github.com/The-AgenticFlow/openflows/pull/185))

### Fixed

- *(hooks)* harden lifecycle hooks per review feedback
- *(ci)* resolve clippy warnings and unused dependency in hook consumer
- *(lifecycle-hooks)* recover hook orchestration and replay pending events
- *(doctor)* require external-auth client ID; centralize via CoderConfig ([#219](https://github.com/The-AgenticFlow/openflows/pull/219))
- *(coder)* use Coder external-auth token for workspace git access
- *(doctor)* only compare semantic-version Coder tags, skip floating tags
- *(config)* address PR review feedback ([#204](https://github.com/The-AgenticFlow/openflows/pull/204))

### Other

- *(doctor)* wrap long println to satisfy rustfmt
- *(coder)* update non-code refs to org-scoped v2 models ([#186](https://github.com/The-AgenticFlow/openflows/pull/186))
- Merge branch 'develop' into issue-185-centralize-envconfig

## [1.2.1](https://github.com/The-AgenticFlow/openflows/releases/tag/openflows-1.2.1) - 2026-08-24

### Added

- split releases into openflows and openflows-harness without v prefix
- adopt release-plz with develop-as-default branching
- Add tenant clean command and gate approval system
- feat setup issues
- feat coder provisioner and openflows
- implement Coder integration plan (Phases 1-3)
- embed all orchestration files at compile time with self-healing and version tracking
- add detailed prerequisites to README, refactor output indexing in responses proxy, extract strip_provider_prefix to agent-client
- per-agent GitHub tokens with instance suffix support
- per-agent GitHub tokens, git identity from PAT, and REST client extensions
- *(lore)* commit and push docs PR after generating documentation
- add conflict resolver, enhance vessel/nexus agents, and improve proxy-first routing
- implement VESSEL agent (DevOps & Merge Gate)
- Complete FORGE-SENTINEL pair lifecycle with SENTINEL review certification
- Switch real_test to use ForgePairNode for full SENTINEL lifecycle
- complete Phase 4 orchestration with robust JSON extraction and worker logging
- *(phase2)* implemented nexus
- *(nexus)* add real E2E test, mcp-proxy bridge, and enhanced logging
- implement hosted MCP bridge, nexus E2E tests, and contributing guide

### Fixed

- improve quick-start UX, default env vars, and GitHub sign-ups
- *(ci)* resolve failing checks (fmt, clippy, dead-code, unused dep, typo)
- *(nexus)* rotate forge chats bound to stale/dead workspaces
- *(orchestration)* spawn SENTINEL for planning-gate review
- remove needless borrow flagged by clippy -D warnings
- agent orchestration pipeline - 6 critical bugs
- fix nexus infiite loop and document
- cycle nexus→forge_pair while workers are busy, not stop
- break nexus<->forge_pair infinite loop and fix v2 registry slot derivation
- use --env-file /dev/null for docker-compose to avoid parsing project .env
- add Coder bootstrap and docker-compose auto-start to running binary
- vessel node now uses resolver registry and searches OPENFLOWS_HOME first
- comprehensive defense against registry/token/path bugs
- reject candidates inside orchestration/agent to prevent doubled paths
- address PR reviews — backup from OPENFLOWS_HOME, extract helper, add reset recovery hint
- preserve user registry.json during install and preserve on reset
- prioritize OPENFLOWS_HOME registry and gracefully skip lore on token error
- set executable permissions on .sh files and restore trap in install.sh
- address gitar review feedback
- track all orchestration files in git and remove duplicate profile
- OPENFLOWS_HOME used directly without double-appending /.openflows in load_env()
- standardize on OPENFLOWS_HOME, propagate .env parse errors, remove /tmp fallback
- load .env from ~/.openflows, install orchestration config, and improve Quick Start
- create temp registry.json in nexus e2e test instead of reading from workspace
- resolve nexus e2e test by using CARGO_MANIFEST_DIR for workspace-relative paths
- clippy warnings, format, and test fixes for CI
- fixes
- fix codex
- implementing plugin module for codex
- resolve clippy warnings - implement FromStr trait and remove dead code
- resolve CI failures (format, clippy, license)
- *(lore)* route merged PRs to lore before CI fixes
- *(ci)* resolve 4 failing CI checks — spelling, clippy, format, cargo deny
- *(ci)* resolve forge CI fix loop — watchdog kills stalled pairs, structured failure detail, worklog enforcement
- resolve clippy, fmt, cargo-deny, and semver-checks CI failures
- resolve merge conflict infinite loop — unrelated histories, force-push, duplicate PR prevention
- Add ticket state machine, remove nested tokio runtime, and fix worktree cleanup
- *(forge)* Claude Code stdin prompt and workspace isolation
- fixing compilation of the demo

### Other

- bump openflows and openflows-harness to 1.2.1
- release v1.2.0
- Rustfmt
- rename orchestration volume to artifacts
- sentinel <-> nexus <-> forge
- Task 2: Implement Nexus A2A relay server module (Refs #143)
- v1.1.8 — fix worker workspaces booting without openflows-harness
- Rustfmt
- doc clarity
- Address review: stop double-tenant key in `openflows gate` CLI (PR #141)
- Add GitHub token setup and harness improvements for Coder workspaces
- retry CI checks
- fix formatting and trailing whitespace
- Nexus reconciliation
- workspace ssh and provisioning
- coder provisioner
- Coder provisioner
- bump version to 1.1.6
- call current_exe() once and derive parent dirs from single result
- Rustfmt
- Rustfmt
- fix formatting for CI
- bump version to 1.0.16
- add codex E2E test variants for nexus
- agent config
- rename all agentflow binaries to openflows
- Add support for OpenAI-compatible providers via CODEX_USE_SSE
- Fix CI checks: fmt, clippy, failing nexus_e2e test, doc warning
- shared dir in worktree, MCP timeouts, Claude provisioning, RUST_LOG support
- Merge remote-tracking branch 'origin/main' into my-fix-branch-38
- Fix LLM routing: model-aware FallbackClient, startup diagnostics, ForgePairNode migration
- Fix test: use GITHUB_PERSONAL_ACCESS_TOKEN in nexus_e2e test
- Fix clippy warning: implement FromStr trait for CliBackend
- Merge branch '38.2-feat-add-codex-cli-as-a-configurable-agent-backend-decouple-from-claude-code' into main
- project cleanup, rename binary to agentflow, and update docs
- apply cargo fmt formatting
- resolve conflicts with main, preserving all VESSEL agent and main features
- Rename sprintless/ to orchestration/ for clarity
- Move .agent directory into sprintless/agent and update all references
- Phase 2: Add agent-client crate, refactor github to MCP bridge, and align workspace

## [1.2.0](https://github.com/The-AgenticFlow/openflows/releases/tag/v1.2.0) - 2026-08-21

### Added

- adopt release-plz with develop-as-default branching
- Add tenant clean command and gate approval system
- feat setup issues
- feat coder provisioner and openflows
- implement Coder integration plan (Phases 1-3)
- embed all orchestration files at compile time with self-healing and version tracking
- add detailed prerequisites to README, refactor output indexing in responses proxy, extract strip_provider_prefix to agent-client
- per-agent GitHub tokens with instance suffix support
- per-agent GitHub tokens, git identity from PAT, and REST client extensions
- *(lore)* commit and push docs PR after generating documentation
- add conflict resolver, enhance vessel/nexus agents, and improve proxy-first routing
- implement VESSEL agent (DevOps & Merge Gate)
- Complete FORGE-SENTINEL pair lifecycle with SENTINEL review certification
- Switch real_test to use ForgePairNode for full SENTINEL lifecycle
- complete Phase 4 orchestration with robust JSON extraction and worker logging
- *(phase2)* implemented nexus
- *(nexus)* add real E2E test, mcp-proxy bridge, and enhanced logging
- implement hosted MCP bridge, nexus E2E tests, and contributing guide

### Fixed

- improve quick-start UX, default env vars, and GitHub sign-ups
- *(ci)* resolve failing checks (fmt, clippy, dead-code, unused dep, typo)
- *(nexus)* rotate forge chats bound to stale/dead workspaces
- *(orchestration)* spawn SENTINEL for planning-gate review
- remove needless borrow flagged by clippy -D warnings
- agent orchestration pipeline - 6 critical bugs
- fix nexus infiite loop and document
- cycle nexus→forge_pair while workers are busy, not stop
- break nexus<->forge_pair infinite loop and fix v2 registry slot derivation
- use --env-file /dev/null for docker-compose to avoid parsing project .env
- add Coder bootstrap and docker-compose auto-start to running binary
- vessel node now uses resolver registry and searches OPENFLOWS_HOME first
- comprehensive defense against registry/token/path bugs
- reject candidates inside orchestration/agent to prevent doubled paths
- address PR reviews — backup from OPENFLOWS_HOME, extract helper, add reset recovery hint
- preserve user registry.json during install and preserve on reset
- prioritize OPENFLOWS_HOME registry and gracefully skip lore on token error
- set executable permissions on .sh files and restore trap in install.sh
- address gitar review feedback
- track all orchestration files in git and remove duplicate profile
- OPENFLOWS_HOME used directly without double-appending /.openflows in load_env()
- standardize on OPENFLOWS_HOME, propagate .env parse errors, remove /tmp fallback
- load .env from ~/.openflows, install orchestration config, and improve Quick Start
- create temp registry.json in nexus e2e test instead of reading from workspace
- resolve nexus e2e test by using CARGO_MANIFEST_DIR for workspace-relative paths
- clippy warnings, format, and test fixes for CI
- fixes
- fix codex
- implementing plugin module for codex
- resolve clippy warnings - implement FromStr trait and remove dead code
- resolve CI failures (format, clippy, license)
- *(lore)* route merged PRs to lore before CI fixes
- *(ci)* resolve 4 failing CI checks — spelling, clippy, format, cargo deny
- *(ci)* resolve forge CI fix loop — watchdog kills stalled pairs, structured failure detail, worklog enforcement
- resolve clippy, fmt, cargo-deny, and semver-checks CI failures
- resolve merge conflict infinite loop — unrelated histories, force-push, duplicate PR prevention
- Add ticket state machine, remove nested tokio runtime, and fix worktree cleanup
- *(forge)* Claude Code stdin prompt and workspace isolation
- fixing compilation of the demo

### Other

- Rustfmt
- rename orchestration volume to artifacts
- sentinel <-> nexus <-> forge
- Task 2: Implement Nexus A2A relay server module (Refs #143)
- v1.1.8 — fix worker workspaces booting without openflows-harness
- Rustfmt
- doc clarity
- Address review: stop double-tenant key in `openflows gate` CLI (PR #141)
- Add GitHub token setup and harness improvements for Coder workspaces
- retry CI checks
- fix formatting and trailing whitespace
- Nexus reconciliation
- workspace ssh and provisioning
- coder provisioner
- Coder provisioner
- bump version to 1.1.6
- call current_exe() once and derive parent dirs from single result
- Rustfmt
- Rustfmt
- fix formatting for CI
- bump version to 1.0.16
- add codex E2E test variants for nexus
- agent config
- rename all agentflow binaries to openflows
- Add support for OpenAI-compatible providers via CODEX_USE_SSE
- Fix CI checks: fmt, clippy, failing nexus_e2e test, doc warning
- shared dir in worktree, MCP timeouts, Claude provisioning, RUST_LOG support
- Merge remote-tracking branch 'origin/main' into my-fix-branch-38
- Fix LLM routing: model-aware FallbackClient, startup diagnostics, ForgePairNode migration
- Fix test: use GITHUB_PERSONAL_ACCESS_TOKEN in nexus_e2e test
- Fix clippy warning: implement FromStr trait for CliBackend
- Merge branch '38.2-feat-add-codex-cli-as-a-configurable-agent-backend-decouple-from-claude-code' into main
- project cleanup, rename binary to agentflow, and update docs
- apply cargo fmt formatting
- resolve conflicts with main, preserving all VESSEL agent and main features
- Rename sprintless/ to orchestration/ for clarity
- Move .agent directory into sprintless/agent and update all references
- Phase 2: Add agent-client crate, refactor github to MCP bridge, and align workspace
