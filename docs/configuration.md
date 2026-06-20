# Configuration

This document explains the shape of `.env` and how to keep setup organized.

## Principle

Keep the default onboarding path as small as possible:

- one CLI backend
- one GitHub repository
- one GitHub token
- one LLM/provider key

Everything else should be optional and clearly marked as advanced.

## Recommended Default

The easiest first setup is:

```env
DEFAULT_CLI=claude
CLAUDE_PATH=claude
ANTHROPIC_API_KEY=your_anthropic_api_key_here
GITHUB_REPOSITORY=owner/repo
GITHUB_PERSONAL_ACCESS_TOKEN=your_github_pat_here
```

That combination matches the default source code path and the guided setup wizard.

## Required Values

These values are needed for the runtime to do real work:

- `GITHUB_REPOSITORY`
- `GITHUB_PERSONAL_ACCESS_TOKEN`
- one provider key, such as `ANTHROPIC_API_KEY`, depending on the backend you choose

## Optional Values

These are useful but not required for a first run:

- `CLAUDE_PATH` or `CODEX_PATH`
- `REDIS_URL`
- `GITHUB_MCP_CMD`
- `GITHUB_MCP_TYPE`
- per-agent GitHub tokens
- provider-specific model overrides

## Backend Choice

The project supports multiple CLI backends, but the docs should treat them as alternatives, not equal starting points.

- `claude` is the default backend
- `codex` is supported for advanced or explicit OpenAI-style CLI workflows

If you switch the backend, make sure the path and provider settings match that choice.

## Provider Choices

### Anthropic direct

Use this for the default onboarding path:

```env
ANTHROPIC_API_KEY=...
DEFAULT_CLI=claude
```

### OpenAI or Fireworks

Use these when you intentionally want a different provider:

```env
OPENAI_API_KEY=...
OPENAI_BASE_URL=...
```

or

```env
FIREWORKS_API_KEY=...
FIREWORKS_MODEL=...
OPENAI_BASE_URL=https://api.fireworks.ai/inference/v1
```

### Proxy mode

Use proxy mode only when you need Anthropic-format requests translated to an OpenAI-compatible gateway:

```env
PROXY_URL=http://localhost:8765/v1
GATEWAY_URL=https://api.fireworks.ai/inference/v1/
```

## Shared State

By default, OpenFlows uses in-memory state. If you want persistence across restarts, set:

```env
REDIS_URL=redis://localhost:6379
```

## Workspace Location

The workspace root defaults to:

```env
AGENTFLOW_WORKSPACE_ROOT=~/.agentflow/workspaces
```

That directory contains cloned repositories and orchestration artifacts for each run.

## When To Edit The Registry

You usually do not need to edit `orchestration/agent/registry.json` for a first run.

Only change it if you need:

- custom agent counts
- a different default CLI backend
- per-agent model routing
- per-agent GitHub tokens

## Related Docs

- [Getting Started](getting-started.md)
- [CLI backend configuration](cli-backend-configuration.md)
- [System behavior](architecture/system-behavior.md)
