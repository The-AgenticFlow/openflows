# OpenFlows Public Roadmap

> Building the future of autonomous software development

**Last Updated:** May 15, 2026  
**Tracking:** [Issue #53](https://github.com/The-AgenticFlow/openflows/issues/53)

---

## 🗺️ Visual Roadmap

```
┌─────────────────────────────────────────────────────────────────────────┐
│                         OPENFLOWS ROADMAP                               │
├─────────────────────────────────────────────────────────────────────────┤
│                                                                         │
│  🟢 NOW (May 2026)       🟡 NEXT (Q2 2026)       🔵 LATER (Q3 2026)     │
│  ├─ PR-Issue Auto-close  ├─ AgentFlow Hub        ├─ Homebrew tap        │
│  ├─ Milestone Awareness  ├─ Docker images        ├─ crates.io publish   │
│  ├─ Sprint Reviews        ├─ Static binaries      ├─ .deb/.rpm packages  │
│  └─ 10+ Contributors      └─ Install script        └─ Nix flake          │
│                                                                         │
└─────────────────────────────────────────────────────────────────────────┘
```

---

## 🟢 Now (Active - May 2026)

### PR-Issue Auto-close
- **Status:** 🔄 In Progress  
- **Issue:** [#49](https://github.com/The-AgenticFlow/openflows/issues/49)  
- **Description:** Enforce `Closes #N` in PRs so issues auto-close on merge. VESSEL pre-merge check.  
- **Target:** May 20, 2026  
- **How to Help:** Review the PR template implementation, test with sample issues

### Milestone Awareness
- **Status:** 🔄 In Progress  
- **Issue:** [#52](https://github.com/The-AgenticFlow/openflows/issues/52)  
- **Description:** NEXUS becomes milestone-aware for sprint reviews and priority-based assignment  
- **Target:** May 25, 2026  
- **How to Help:** Design milestone data structures, implement in NEXUS agent

### Sprint Reviews
- **Status:** 🔄 In Progress  
- **Issue:** [#52](https://github.com/The-AgenticFlow/openflows/issues/52)  
- **Description:** NEXUS pauses at milestone boundaries for human review before continuing  
- **Target:** May 31, 2026  
- **How to Help:** Build notification system, create review UI/dashboard

### 10+ Contributors Goal
- **Status:** 🔄 In Progress  
- **Description:** Build contributor base following the Steinberger model (OpenClaw's growth path)  
- **Target:** May 31, 2026  
- **How to Help:** Pick up [good first issues](https://github.com/The-AgenticFlow/openflows/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22), spread the word

---

## 🟡 Next (Q2 2026 - June)

### AgentFlow Hub Marketplace
- **Status:** ⏳ Planned  
- **Issue:** [#50](https://github.com/The-AgenticFlow/openflows/issues/50)  
- **Description:** Central registry for skills, plugins, and agents (local + remote)  
- **Target:** June 15, 2026  
- **How to Help:** Design hub architecture, implement CLI commands

### CLI Integration
- **Status:** ⏳ Planned  
- **Issue:** [#50](https://github.com/The-AgenticFlow/openflows/issues/50)  
- **Description:** `agentflow hub search/install/list/connect` commands  
- **Target:** June 20, 2026  
- **How to Help:** Build CLI interface, API client for hub

### Docker Images
- **Status:** ⏳ Planned  
- **Issue:** [#51](https://github.com/The-AgenticFlow/openflows/issues/51)  
- **Description:** Multi-arch container images with semver tagging on ghcr.io  
- **Target:** June 10, 2026  
- **How to Help:** Write Dockerfiles, set up GitHub Actions for builds

### Static Binaries
- **Status:** ⏳ Planned  
- **Issue:** [#51](https://github.com/The-AgenticFlow/openflows/issues/51)  
- **Description:** Cross-compiled MUSL binaries on GitHub Releases  
- **Target:** June 15, 2026  
- **How to Help:** Set up cross-compilation toolchain, GitHub Actions

### Install Script
- **Status:** ⏳ Planned  
- **Issue:** [#51](https://github.com/The-AgenticFlow/openflows/issues/51)  
- **Description:** `curl -sL https://get.openflows.dev | sh` one-liner  
- **Target:** June 20, 2026  
- **How to Help:** Write install script, test on different platforms

---

## 🔵 Later (Q3 2026 - July-September)

### Homebrew Tap
- **Status:** ⏳ Planned  
- **Issue:** [#51](https://github.com/The-AgenticFlow/openflows/issues/51)  
- **Description:** `brew tap The-AgenticFlow/tap && brew install openflows`  
- **Target:** July 15, 2026

### crates.io Publish
- **Status:** ⏳ Planned  
- **Issue:** [#51](https://github.com/The-AgenticFlow/openflows/issues/51)  
- **Description:** `cargo install openflows`  
- **Target:** July 31, 2026

### .deb / .rpm Packages
- **Status:** ⏳ Planned  
- **Issue:** [#51](https://github.com/The-AgenticFlow/openflows/issues/51)  
- **Description:** System packages for enterprise Linux  
- **Target:** August 31, 2026

### Nix Flake
- **Status:** ⏳ Planned  
- **Issue:** [#51](https://github.com/The-AgenticFlow/openflows/issues/51)  
- **Description:** `nix run github:The-AgenticFlow/openflows`  
- **Target:** September 30, 2026

---

## ✅ Completed Milestones

### Foundation Layer (Complete)
- ✅ PocketFlow Core — Flow engine and shared state
- ✅ Agent Client — Multi-provider LLM client with MCP
- ✅ Config System — Shared state types and registry
- ✅ GitHub Client — REST API for issues/PRs/CI
- ✅ Pair Harness — Worktree management and process spawning

### Agent Layer (Complete)
- ✅ NEXUS — Orchestrator and Scrum Master
- ✅ FORGE — Builder and Senior Engineer
- ✅ SENTINEL — Security Auditor and Reviewer
- ✅ VESSEL — DevOps and Merge Gatekeeper
- ✅ LORE — Documenter and Technical Writer

### Infrastructure (Complete)
- ✅ LiteLLM Proxy — Per-agent model routing
- ✅ Redis SharedStore — Inter-agent communication
- ✅ Docker Compose — Full stack deployment
- ✅ GitHub Actions — CI/CD pipeline

---

## 🤝 How to Contribute

### Pick Your Path

**🌱 First Time Contributors**
- Look for [good first issues](https://github.com/The-AgenticFlow/openflows/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22)
- Start with documentation improvements
- Join our [Discord](https://discord.gg/Zf6PTQAgE) for help

**🦀 Rust Developers**
- Agent implementations (NEXUS, FORGE, VESSEL)
- Performance optimizations
- Testing infrastructure

**📊 DevOps Engineers**
- Docker improvements
- CI/CD enhancements
- Distribution packaging

**🎨 Designers & Writers**
- Documentation improvements
- Tutorial creation
- Website and branding

### Get Started
1. Read [CONTRIBUTING.md](CONTRIBUTING.md)
2. Join [Discord](https://discord.gg/Zf6PTQAgE)
3. Pick an issue from this roadmap
4. Comment on the issue to get assigned
5. Start building!

---

## 📊 Progress Metrics

| Metric | Current | Target (May 31) |
|--------|---------|-----------------|
| Contributors | 5 | 10+ |
| Stars | 10 | 50+ |
| Forks | 2 | 15+ |
| Closed Issues | 25 | 40+ |
| PRs Merged | 15 | 30+ |

---

## 🔄 Updates

This roadmap is updated weekly. Watch this issue for changes:
- Major milestones moved to "Completed"
- New features added to "Next" or "Later"
- Target dates adjusted based on progress

**Last update:** May 15, 2026 by @Christiantyemele

---

*Built by [The-AgenticFlow](https://github.com/The-AgenticFlow) — Turning GitHub issues into merged PRs, autonomously.*
