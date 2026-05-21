# 🤝 Contributing to OpenFlows

> Welcome! We're building the future of autonomous software development, and we need your help.

[![Stars](https://img.shields.io/github/stars/The-AgenticFlow/OpenFlows?style=social)](https://github.com/The-AgenticFlow/OpenFlows/stargazers)
[![Discord](https://img.shields.io/discord/123456789?color=7289da&label=discord&logo=discord&logoColor=white)](https://discord.gg/Zf6PTQAgE)
[![License](https://img.shields.io/badge/license-MIT-blue.svg)](LICENSE)

## 🎯 What is OpenFlows?

OpenFlows is an **autonomous AI development team** that turns GitHub issues into working code with pull requests — all without human intervention.

Think of it as having a team of AI agents (NEXUS, FORGE, VESSEL, SENTINEL, LORE) that collaborate to build software just like a human team would.

**🌐 Official site:** [openflows.dev](https://openflows.dev)

---

## 🚀 Quick Start for Contributors

### Step 1: Fork & Clone
```bash
git clone https://github.com/YOUR_USERNAME/OpenFlows.git
cd OpenFlows
```

### Step 2: Set Up Environment
```bash
# Copy environment template
cp .env.example .env

# Install Rust (if not already installed)
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh

# Install Claude Code CLI (required for Forge workers)
npm install -g @anthropic-ai/claude-code
claude auth login
```

### Step 3: Configure
Edit `.env` with your API keys:
```bash
# Required: GitHub access
GITHUB_PERSONAL_ACCESS_TOKEN=ghp_xxxxxxxxxxxxxxxxxxxx
GITHUB_REPOSITORY=your-org/your-repo

# Required: Path to Claude CLI
CLAUDE_PATH=$(which claude)

# Choose your LLM mode:
# Option A: Proxy mode (recommended)
PROXY_URL=http://localhost:8080/v1
PROXY_API_KEY=your-key

# Option B: Direct mode (fallback)
ANTHROPIC_API_KEY=your-key
OPENAI_API_KEY=your-key
```

### Step 4: Build & Test
```bash
# Build the project
cargo build --release

# Run tests
cargo test --workspace

# Run the demo (uses mock servers, no API keys needed!)
cargo run -p openflows --bin demo
```

---

## 🎓 New Contributor? Start Here!

### Good First Issues
Look for issues labeled:
- `good first issue` — Simple tasks to get started
- `help wanted` — We need community help
- `documentation` — Improve docs, no code required
- `rust` — Rust-specific improvements

### First Contribution Ideas
1. **Fix a typo** in documentation
2. **Add a test** for existing functionality
3. **Improve error messages** for better UX
4. **Add logging** to help with debugging
5. **Write a tutorial** based on your experience

---

## 📋 Contribution Workflow

### 1. Find or Create an Issue
- Check [existing issues](https://github.com/The-AgenticFlow/OpenFlows/issues)
- Comment on an issue to express interest
- Or [create a new issue](https://github.com/The-AgenticFlow/OpenFlows/issues/new) for bugs/features

### 2. Get Assigned
- Wait for a maintainer to assign you
- This prevents duplicate work
- We'll add you as a contributor once you complete your first PR

### 3. Create a Branch
```bash
git checkout -b feature/your-feature-name
# or
git checkout -b fix/issue-description
```

### 4. Make Changes
- Follow Rust best practices
- Add tests for new functionality
- Update documentation if needed
- Keep commits focused and atomic

### 5. Test Your Changes
```bash
# Run all tests
cargo test --workspace

# Check formatting
cargo fmt -- --check

# Run linter
cargo clippy -- -D warnings

# Run the demo to verify
cargo run -p openflows --bin demo
```

### 6. Commit & Push
```bash
git add .
git commit -m "feat: add feature description"
git push origin feature/your-feature-name
```

**Commit message format:**
- `feat:` — New feature
- `fix:` — Bug fix
- `docs:` — Documentation changes
- `test:` — Adding tests
- `refactor:` — Code refactoring
- `chore:` — Maintenance tasks

### 7. Create a Pull Request
- Go to GitHub and create a PR from your branch
- Fill out the PR template
- Link to the issue you're fixing
- Request review from maintainers

---

## 🏗️ Project Architecture

### Core Components

| Component | Role | Description |
|-----------|------|-------------|
| **NEXUS** | Orchestrator | Assigns work to agents, manages workflow |
| **FORGE** | Developer | Spawns Claude Code to implement code |
| **VESSEL** | Merger | Merges approved PRs, handles CI |
| **SENTINEL** | Reviewer | Evaluates code quality |
| **LORE** | Documenter | Writes docs and ADRs |

### Key Concepts

**SharedStore**: Central key-value store where agents exchange state
- `tickets` — GitHub issues converted to work items
- `worker_slots` — Agent worker assignments
- `pending_prs` — Pull requests awaiting merge

**PocketFlow**: The engine that executes the agent graph and manages state transitions.

For deep technical details, see:
- [SharedStore Documentation](docs/shared-store.md)
- [Architecture Overview](docs/architecture.md)
- [API Reference](docs/api.md)

---

## 🧪 Testing Guide

### Unit Tests
```bash
# Run all unit tests
cargo test --workspace

# Run tests for a specific crate
cargo test -p agent-nexus
cargo test -p agent-forge
```

### End-to-End Tests
```bash
# Test Nexus decision making
cargo test -p agent-nexus --test nexus_e2e

# Test Forge suspension logic
cargo test -p agent-forge --test forge_claude_e2e
```

### Demo Mode (No API Keys Needed)
```bash
# Terminal 1: Start mock LLM
python3 scripts/mock_llm.py

# Terminal 2: Run demo
cargo run -p openflows --bin demo
```

---

## 🔧 Development Tips

### Running in Different Modes

**Mock Mode** (safe, no API keys):
```bash
cargo run -p openflows --bin demo
```

**Real Mode** (connects to live LLMs):
```bash
cargo run -p openflows --bin agentflow
```

**Dashboard** (monitor workers):
```bash
cargo run -p openflows --bin dashboard
```

### Common Issues & Solutions

**Issue**: `Failed to spawn FORGE process`
**Solution**: Check `CLAUDE_PATH` in `.env` points to valid `claude` binary

**Issue**: Build failures
**Solution**: 
```bash
rustup update
cargo clean
cargo build --release
```

**Issue**: Test failures
**Solution**: Ensure all prerequisites are installed (Rust, Node.js, Python 3)

---

## 🎖️ Recognition

Contributors will be:
- Listed in [CONTRIBUTORS.md](CONTRIBUTORS.md)
- Mentioned in release notes
- Added to the organization after 3+ quality contributions

---

## 💬 Get Help

- **Discord**: [Join our community](https://discord.gg/Zf6PTQAgE)
- **GitHub Discussions**: [Ask questions](https://github.com/The-AgenticFlow/OpenFlows/discussions)
- **Issues**: [Report bugs](https://github.com/The-AgenticFlow/OpenFlows/issues)

---

## 📚 Additional Resources

- [Official Website](https://openflows.dev)
- [Tutorial](TUTORIAL.md)
- [Demo Walkthrough](DEMO.md)
- [Packaging Guide](PACKAGING.md)
- [Security Policy](orchestration/agent/standards/SECURITY.md)

---

## 🙏 Thank You!

Every contribution matters — whether it's fixing a typo, adding a test, or building a major feature. You're helping shape the future of autonomous software development.

**Ready to contribute?** [Pick an issue](https://github.com/The-AgenticFlow/OpenFlows/issues?q=is%3Aissue+is%3Aopen+label%3A%22good+first+issue%22) and let's build something amazing together! 🚀
