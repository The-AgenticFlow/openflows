# Getting Started

This guide is the recommended onboarding path for new collaborators.

## What OpenFlows Is

OpenFlows is a self-hosted autonomous workflow for GitHub issues. You run it locally or in your own infrastructure, point it at a repository, and it coordinates agent roles to plan, implement, review, and merge work.

It is not a separate hosted service. Your machine or server runs the runtime, and the runtime talks to GitHub plus whichever AI provider you configure.

## Recommended Setup

If you want the least confusing path, use the default local setup directly from the repository:

1. Run `cargo run --bin openflows-setup`.
2. Run `cargo run --bin openflows-doctor`.
3. Start `cargo run --bin openflows`.

```bash
cargo run --bin openflows-setup
cargo run --bin openflows-doctor
cargo run --bin openflows
```

## Prerequisites

Before setup, make sure you have:

- Git
- Rust toolchain
- Node.js 18+
- A GitHub personal access token
- An Anthropic API key for the default onboarding path

You will also need the Claude Code CLI available on your machine if you use the default `claude` backend.

## Docker

Docker is optional and is most useful if you want bundled local services like Redis or a proxy.

```bash
docker compose up -d
```

This is not the primary path for first-time onboarding. The project is easier to understand if you start with the CLI setup first.

## First Run

1. Copy the example environment file:

```bash
cp .env.example .env
```

2. Fill in the minimum required values:

- `GITHUB_REPOSITORY`
- `GITHUB_PERSONAL_ACCESS_TOKEN`
- `ANTHROPIC_API_KEY`

3. Start the guided setup:

```bash
cargo run --bin openflows-setup
```

4. Run the doctor:

```bash
cargo run --bin openflows-doctor
```

5. Start the orchestrator:

```bash
cargo run --bin openflows
```

## What Happens Next

Once the runtime starts, it:

- loads `.env`
- reads the agent registry
- clones or updates the target repository
- creates a workspace under `~/.agentflow/workspaces/`
- polls GitHub for issues
- assigns work to agent roles
- opens pull requests when work is complete

## Choosing A Mode

For most new collaborators, the default direct Anthropic setup is the best place to start.

Use the advanced modes only when you need them:

- `Codex` backend if you explicitly want OpenAI-style CLI execution
- `Fireworks` if you want a provider that speaks the OpenAI-compatible format
- `Proxy` mode if you need to translate Anthropic-format requests to another gateway
- `Redis` if you want shared state persistence across restarts

Those options are documented in [Configuration](configuration.md).

## Troubleshooting

- If the CLI cannot find `claude`, set `CLAUDE_PATH` in `.env`.
- If setup complains about GitHub access, re-check the PAT and repository name.
- If the doctor reports missing values, fix `.env` first and rerun it.
- If you want a deeper system walkthrough, read [docs/demo.md](demo.md).

## Next Reading

- [Configuration](configuration.md)
- [CLI backend configuration](cli-backend-configuration.md)
- [System behavior](architecture/system-behavior.md)
