# OpenFlows — Quick Start

Turn GitHub issues into reviewed pull requests, on your own machine. For what OpenFlows is, see the [README](README.md).

## You need

- Docker (running), Rust (`cargo`), `curl`, `python3`
- A GitHub account that can create apps on the repo's owner (user or org)
- An API key for an LLM provider (Anthropic, OpenAI, OpenRouter or Google)

## Run

```bash
./scripts/setup.sh owner/repo
```

That is the whole setup. The script is safe to re-run: it checks what is already done and continues from there. If something is missing, it stops and tells you exactly what to do.

It will:

1. Check your machine (and install the `coder` CLI if missing).
2. Create `.env` and generate secrets.
3. Create the **GitHub App** — your browser opens, you click **Create GitHub App**, then install it on your repo.
4. Start Redis and Coder in Docker.
5. Sign you in to Coder with **your GitHub account** (you become the owner) and save an API token you create in the dashboard (one paste).
6. Add your **LLM** provider and model (asks for the API key).
7. Build binaries and push the workspace templates (first run takes about 10 minutes).
8. Connect your repo. You click **Authorize** once to link GitHub to your Coder account; the script opens the page and waits.

The script asks how many **FORGE-SENTINEL pairs** (the fleet) to run: one pair works one issue at a time, N pairs work N issues in parallel and use more LLM tokens. Start with `1`. To skip the question, pass it: `./scripts/setup.sh owner/repo --fleet 3`. Other option: `--name TEAM` (default: `owner-repo`).

## Use it

Open an issue in your repo. OpenFlows picks it up, writes code, reviews it and opens a pull request.

- Dashboard: <http://localhost:7080>
- Health check: `./scripts/prod.sh doctor`
- Add another repo: `./scripts/prod.sh tenant owner/repo --name team --fleet 1`

## Something went wrong?

Re-run `./scripts/setup.sh owner/repo` after fixing the problem it printed. Your Coder token lasts 90 days; re-running the script refreshes it. For manual steps, hooks, roles, configuration and troubleshooting, see [docs/setup-reference.md](docs/setup-reference.md).
