# Deterministic system test fixtures (task 3)

These are the external-service adapters for the future complete system E2E. They are test programs, not production services. This stage does **not** run the NEXUS controller, Coder chats, lifecycle hooks, human approval, or an issue-to-merge OpenFlows journey. Connecting those production paths is task 4.

## Run the contracts

```bash
bash tests/e2e/system/run.sh
```

Requires Linux, Python 3.12, Git, Rust, Docker, and GNU `timeout`. The runner checks the **production OpenFlows GitHub client** against a local HTTP server and real Git remote, then runs fixture contracts in a disposable container with network egress disabled. `System E2E fixture contracts` is required by `OpenFlows Merge Gate`.

The fixtures use only Python's standard library and Git. No model API key, GitHub account, production configuration, or Docker socket mount is needed inside the fixture container.

For faster development:

```bash
python3 -m unittest discover -s tests/e2e/system -v
cargo test --locked -p github --test system_fixture_contract
```

## GitHub and Git

```bash
python3 tests/e2e/system/fixtures.py \
  --service github --root /tmp/openflows-github-fresh \
  --ready-file /tmp/openflows-github-ready.json
```

Use a fresh `--root` directory. The ready file contains the HTTP URL. Request `GET /repos/test/repo` with a disposable credential to discover `clone_url`. The fixture seeds `main` with a deliberately broken `answer.txt` containing `41` and starts a real Git daemon on an allocated loopback port.

The API supports issue creation/listing/updates/comments; PR creation/listing/details; reviews; exact-commit checks and status queries; and merge commits. Unsupported endpoints, inline review comments, and squash/rebase merges are rejected explicitly. This is a limited API adapter, not a GitHub emulator. New production requests must receive explicit implementation and contract coverage.

`GITHUB_API_BASE` can redirect OpenFlows to this server. Separate disposable test credentials represent callers:

| Credential | Identity | Restricted operation |
| --- | --- | --- |
| `ci-forge-token` | FORGE | Creates PRs |
| `ci-sentinel-token` | SENTINEL | Submits reviews |
| `ci-vessel-token` | VESSEL | Requests merges |
| `ci-runner-token` | Independent CI | Publishes acceptance checks |
| `ci-operator-token` | Scenario driver | Creates issues and submits operator reviews |

These are deliberately public fixture credentials, not secrets or a security boundary against a malicious caller who knows the test source. Caller separation tests that the normal workflow uses the correct identity. The fixture must remain isolated from production and the public network. Git transport permits pushes inside the disposable environment; it does not model GitHub repository authentication or branch protection. Task 4 must assert that OpenFlows requests the merge through the normal VESSEL path.

A branch push updates the API's PR head from real Git. Merge requires the requested SHA to equal that head, an independently published successful acceptance check, and SENTINEL's latest non-comment review to approve that head. The adapter deliberately does not enforce OpenFlows' human gate: task 4 must prove that the production controller holds the merge until human approval.

An accepted merge executes real `git merge --no-ff`, then updates the base ref with compare-and-swap. A conflict cannot report a successful merge. The contracts inspect the resulting branch and ancestry and run acceptance again against the returned merge SHA.

## Independent acceptance runner

```bash
python3 tests/e2e/system/run_ci.py \
  --api http://127.0.0.1:PORT --remote git://127.0.0.1:GIT_PORT/repo.git \
  --sha EXACT_COMMIT_SHA --oracle tests/e2e/system/acceptance.sh \
  --artifacts /tmp/openflows-ci-evidence
```

The runner publishes an in-progress check, creates its own disposable checkout of the exact SHA, executes the trusted oracle, saves exit/output/head evidence, and publishes the actual result. Clone or checkout errors never become successful checks. Acceptance timeout is 20 seconds.

The oracle lives **outside the candidate repository** and requires `42`. A candidate's replacement `acceptance.sh` cannot override it. This runner exercises real commands; it does not simulate GitHub Actions execution.

## Scripted model provider

Create a scenario JSON file:

```json
{
  "e2e-forge": [
    {"tool": "execute", "arguments": {"command": "cat answer.txt"}},
    {"requires": {"contains": "41"}, "text": "The implementation needs repair."}
  ]
}
```

```bash
python3 tests/e2e/system/fixtures.py \
  --service model --root /tmp/openflows-model-fresh \
  --scenario /tmp/scenario.json --ready-file /tmp/openflows-model-ready.json
```

Configure Coder's `openai-compat` provider to use the ready URL plus `/v1`, key `ci-model-key`, and the corresponding scenario model ID. The fixture implements `POST /v1/chat/completions`, including SSE tool-call streaming. Responses API and other routes are intentionally unsupported.

Each model ID identifies one bounded scripted conversation. The example tool name is illustrative: a scenario must use the real tool name and arguments advertised by its caller. The server validates required arguments and primitive types against that advertised schema. It rejects absent or incompatible tools and unknown models. It does not implement a complete JSON Schema validator.

After returning a tool call, the next step requires its matching tool result; `requires.contains` can additionally assert expected output. Repeated identical requests return the same response without advancing the script. The model fixture does not execute tools, edit files, write lifecycle state, publish checks, or merge: the caller must execute returned tool calls. The real Coder execution and role-to-conversation mapping will be validated in task 4.

For Docker-network use, bind with `--host 0.0.0.0 --port 8080` and set `--advertise-host` to the fixture's service name:

- Model: `--advertise-host model-fixture` publishes `http://model-fixture:8080`; Coder's provider URL is `http://model-fixture:8080/v1`.
- GitHub: `--advertise-host github-fixture --git-host 0.0.0.0 --git-port 9418 --remote-host github-fixture` publishes `http://github-fixture:8080` and a Git remote using `github-fixture`.

The bind address and advertised hostname serve different purposes. Without `--advertise-host`, the ready URL defaults to loopback for host-local tests. Readiness metadata is published with an atomic rename so its first visible contents are complete JSON. These Docker options must only be used inside an isolated test network.

## Failure coverage and evidence

The contracts check:

- Missing or unrelated tool results, schema mismatch, unknown routes, and invalid credentials fail.
- A failed acceptance result, missing CI, changed PR head, stale review, or later request for changes blocks merge.
- FORGE's credential cannot publish CI success.
- A candidate cannot replace the trusted acceptance oracle.
- Real Git conflicts leave the base unchanged and the PR unmerged.
- Successful merge responses identify an actual Git commit containing the fix.

Artifacts under `target/ci-artifacts/system-fixtures/<run>/` include HTTP request/response journals (without authorization headers), Git daemon logs, acceptance evidence, container output, and the production-client test output. Rust diagnostics are copied under `production-client/` on success and failure before its temporary directory is deleted, including when a failed Rust contract prevents the later container tests from running. Tests preserve these before deleting temporary repositories. Cleanup removes only the run's named container and image; server shutdown stops its own Git daemon.

The live suite will validate these adapters against real GitHub and a real model provider. Passing these contract tests does not establish production GitHub authentication or model quality.
