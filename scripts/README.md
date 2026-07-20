# Scripts

## Production Commands

All production operations use `./scripts/prod.sh`:

```bash
./scripts/prod.sh run                            # Start controller (always resets Redis first)
./scripts/prod.sh bootstrap                      # Setup Coder + push templates
./scripts/prod.sh tenant owner/repo --name team  # Add a tenant
./scripts/prod.sh doctor                         # Health check
```

### `run` — Start Controller

```bash
./scripts/prod.sh run
```

**Always resets Redis to a clean slate first**, then starts the controller. This ensures no zombie tickets or stale state from previous runs.

### `bootstrap` — One-time Setup

```bash
./scripts/prod.sh bootstrap
```

- Creates admin user in Coder (if not exists)
- Pushes Coder templates (nexus, forge, etc.)
- Verifies LLM and GitHub external auth are configured

### `tenant` — Add a Team

```bash
./scripts/prod.sh tenant owner/repo --name my-team
```

### `doctor` — Health Check

```bash
./scripts/prod.sh doctor
```

---

## Development Helpers

### `reset-controller-state.sh` — Clean Redis

Reset Redis to a clean state:

```bash
./scripts/reset-controller-state.sh --confirm
```

Removes all tickets, workers, and orchestration state.

### `install.sh` — CLI Installer

Installs the `openflows` CLI binary:

```bash
curl -fsSL https://get.openflows.dev | bash
```

---

## Production Architecture

The controller runs inside a **Nexus workspace** provisioned by Coder. The workspace auto-starts the controller via startup_script.

```
openflows bootstrap → Coder pushes templates
openflows tenant add → Coder creates nexus workspace
    ↓
Workspace starts (docker container)
    ↓
Startup script runs:
  → Installs openflows-harness binary
  → Sets up orchestration volume
  → Starts heartbeat daemon
  → Executes: openflows run
    ↓
Controller starts inside workspace
```

---

## Quick Reference

| Command | Description |
|---------|-------------|
| `./scripts/prod.sh run` | Clean slate + start controller |
| `./scripts/prod.sh bootstrap` | Setup Coder + templates |
| `./scripts/prod.sh tenant owner/repo --name team` | Add tenant |
| `./scripts/prod.sh doctor` | Health check |
| `./scripts/reset-controller-state.sh --confirm` | Clean Redis state |