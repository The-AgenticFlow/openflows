# Workspace artifact delivery: inventory

A role workspace cannot boot without three things that do not come from git: the
immutable role assets (persona, skills, commands, standards, hooks), a repository
to work in, and the flow state it shares with the other roles. This page records
where each of them comes from today, per backend, so the shared Docker volume can
be removed or kept deliberately instead of guessed at.

Two of the three are already independent of the volume. The immutable assets are
served by the Manager and installed by digest in hosted mode, and the flow state
lives in the tenant's Redis namespace. What the volume still carries is the
artifact tree for local installations and the golden repository seed on both.

## What a role needs at boot

| Need | Who reads it | Who provides it today |
| --- | --- | --- |
| Persona, skills, commands, standards, registry | Nexus from its own tree, every worker through its agent loop | Artifact bundle |
| Hooks | The worker's agent CLI, through `settings.json` | Artifact bundle |
| A checkout of the tenant repository | Forge, Sentinel, Vessel, Lore | Golden repository seed, then git |
| Dispatch, plans, status, handoffs, gates, verification results | All roles | Tenant Redis namespace |
| Scratch and untracked files | Each workspace only | The workspace volume |

## The artifact bundle

The bundle is compiled into the binary that ships it, so a release cannot serve
assets it did not build. [`crates/workspace-assets/src/lib.rs`](../../../crates/workspace-assets/src/lib.rs)
embeds 52 files:

| Group | Files |
| --- | --- |
| Hooks, per role | 19 (forge 8, sentinel 6, vessel 2, nexus 2, lore 1) |
| Commands | 14 |
| Skills | 9 |
| Personas | 5 |
| Standards | 3 |
| MCP config and `agent/registry.json` | 2 |

`workspace_assets::tree()` lays those 52 files out under `orchestration/`,
mirrors the 19 hooks at `plugin/hooks/<role>/`, and adds one `<role>.skills`
manifest per registry role, for 75 files in the layout the templates read.
`workspace_assets::archive()` serialises that tree into a gzipped tar with sorted
entries and a zeroed mtime, uid and gid, so the same source always produces the
same bytes and `digest()` is a stable identifier for a release.

Delivery differs by mode:

| Mode | Path | Source | Verified |
| --- | --- | --- | --- |
| Hosted | `$HOME/.openflows/bundle` | `GET /api/v1/runtime/artifacts` on the Manager, with the workspace's runtime token | The `x-artifact-digest` header, compared before unpacking |
| Local Docker | `$HOME/.openflows/artifacts`, read-only mount | `plugin/hooks/` and `orchestration/` published into `ARTIFACTS_DIR` by the controller binary at startup | Nothing; the mount is trusted |

[`crates/openflows-manager/src/routes/artifacts.rs`](../../../crates/openflows-manager/src/routes/artifacts.rs)
builds the archive once per process and serves two routes. `/runtime/artifacts` returns the release the
Manager runs with `no-cache`, `/runtime/artifacts/{digest}` returns only that
exact archive with a one-year immutable cache header, and answers 404 with the
available digest when a workspace pins one the Manager does not carry.

`openflows-harness artifacts fetch --dir --manager-url --token` is the client. It
makes three attempts, retries only a network failure or a server error, and
always checks the served archive against the `x-artifact-digest` header before
installing it, so a corrupted or truncated download is caught.

Release pinning is separate from that integrity check, and on by default. The
harness computes the digest of the bundle compiled into its own binary and asks
the Manager for exactly that bundle, so a Manager serving a different one
answers 404 with both digests in the message and the workspace refuses to boot
with assets its code was not built against. Two ways out, both deliberate:
`--digest` pins a bundle named by the caller, and `--any-release` installs
whichever bundle the Manager serves.

The check is on the bundle, not on a release number. A release that changes
nothing under `orchestration/` compiles to the same digest as the one before it,
so a harness from either still installs. What it catches is a Manager whose
assets differ from the harness's, which is the only case where mixing them
changes what a worker reads.

The consequence for operators: the `harness_version` the worker templates
download has to name the release the Manager runs. Deploying one half ahead of
the other makes workspace rebuilds fail loudly once the bundles differ, which is
the intended behaviour and is recorded in the
[production runbook](operator-production-runbook.md).

## The golden repository seed

Nexus creates the tenant volume and keeps one clone in it at
`/home/coder/.openflows/artifacts/repo`. Workers copy that clone into their own
workspace volume, refresh it, then check out the branch they need, so one network
fetch per tenant replaces one per workspace spawn.

| Role | Mount | Seed handling |
| --- | --- | --- |
| Nexus | Read-write, created by `docker_volume.artifacts` | Clones, or fetches and resets a hard, then repairs ownership |
| Forge | Read-only | Copy, refresh, checkout, then a network clone, each bounded by a 45 second timeout (90 for the clone), with a warning that names the failure |
| Sentinel, Vessel, Lore | Read-only | The same steps with no timeouts: the intermediate pull, copy, fetch and checkout are silenced, and a failed clone writes to `/tmp/<role>_clone_err.log` and warns |

The fallback already works in hosted mode: Forge configures a git credential
store from the Manager's runtime credential exchange before it clones, so a
worker without a seed can still clone a private repository, at the cost of one
fetch per workspace instead of one per tenant. Sentinel, Vessel and Lore inherit
the same credential setup, and the same silenced intermediate steps and missing
timeouts that the table above records.

Every worker, Forge included, ends its bootstrap with a guard that exits when
`/home/coder/workspace/.git` is absent, so none of them finishes booting on an
empty workspace. What separates Forge from the other three is timeouts and how
loudly the intermediate steps report, not whether a missing repository is fatal.

## What is already off the volume

| Data | Store | Entry point |
| --- | --- | --- |
| Plans (Forge writes, Sentinel reads) | Tenant Redis namespace | `openflows-harness plan write/read` |
| Status, phase and gates | Tenant Redis namespace | `status`, `gate` |
| Handoffs | Tenant Redis namespace | `handoff write` |
| Verification results | Tenant Redis namespace | `verify` |
| The checkout and scratch files | Per-workspace Docker volume | `/home/coder/workspace` |

So the issue that started this inventory is narrower than it reads. Plans and
handoffs never needed the shared volume, and the immutable assets only need it in
local mode.

## What still needs the volume

Two things, and only the second is a real coupling:

1. **The artifact tree in local mode.** The controller resolves its own tree
   from `OPENFLOWS_HOME`, the executable directory or the working directory,
   materialising any bundled file that is missing, then publishes `plugin/hooks/`
   and `orchestration/` into `ARTIFACTS_DIR` for the workers. Local workers read
   that mounted copy, because only the hosted path fetches from the Manager.
2. **The golden repository seed in both modes.** Hosted workers still mount the
   tenant volume read-only and prefer the seed. Removing the volume therefore
   moves every workspace spawn to a network clone, which is a cost decision, not
   a correctness one.

## Gaps the delivery contract has to close

| Gap | Effect today | Where |
| --- | --- | --- |
| The local tree is materialised once | `ensure_orchestration_dir` writes only missing files and only warns when `.version` differs, so an upgraded controller keeps publishing the previous release's assets until someone runs `--reset-orchestration` | [`binary/src/orchestration.rs`](../../../binary/src/orchestration.rs) |
| The local copy is never verified | Nothing checks that the mounted assets are the ones the running release built, and that path has no digest to check them against | Templates, `ASSETS_DIR` branch |
| No installed-digest marker | The harness reports what it installed, but nothing keeps it where an operator or the Manager can read it | [`crates/openflows-harness/src/artifacts.rs`](../../../crates/openflows-harness/src/artifacts.rs) |
| Hosted workers still mount the volume | A volume that carries only an optimisation is still part of what a hosted workspace has to be isolated from | Templates, `volumes` blocks |
| Unbounded git waits in three roles | A hung pull, fetch or clone stalls Sentinel, Vessel and Lore with no timeout, where Forge bounds each call and warns instead of hanging | [`openflows-sentinel/main.tf`](../../../crates/coder-client/templates/openflows-sentinel/main.tf) and the other two |
| Read-only mounts repaired with `sudo chown` | Nexus repairs ownership of a volume other roles mount read-only, which assumes the same uid on one host | [`openflows-nexus/main.tf`](../../../crates/coder-client/templates/openflows-nexus/main.tf) |

## Options for the repository seed

| Option | Cost | Risk |
| --- | --- | --- |
| Keep the seed as an optional capability, off by default | None for local, one clone per spawn in Kubernetes | Local installations lose their single-fetch behaviour unless they opt in |
| Serve a repository snapshot from the Manager by digest | Storage per tenant repository and a refresh job | Large repositories, LFS objects, and the Manager holding a clone of every customer repository |
| Drop the seed and clone per workspace | One clone per spawn, egress and credentials already required | Spawn latency on large repositories |

Recommendation: make the clone the contract, keep the seed as an optional
capability for installations that want it. The clone path already exists and is
already credentialed, so the seed is an optimisation. Serving snapshots from the
Manager is worth revisiting only if measured spawn latency says the clones
dominate, since it puts customer repositories inside the control plane.

## Paths that stay

Migration keeps every path a role already reads, so a mixed fleet during the
change still boots:

| Path | Purpose |
| --- | --- |
| `/home/coder/.openflows/bundle` | Installed immutables in hosted mode |
| `/home/coder/.openflows/artifacts` | Mounted artifact tree and golden seed in local mode |
| `/home/coder/.openflows/hooks` | Hooks copied out of the artifact tree before the agent loop starts |
| `/home/coder/workspace` | The checkout |

## Test coverage this needs

| Case | Assertion |
| --- | --- |
| Bundle missing | A workspace refuses to start rather than running with an empty persona tree |
| Digest mismatch | A pinned fetch fails and names the digest the Manager serves |
| Stale seed | A worker copies then refreshes, and a branch that only exists upstream is still reachable |
| Server error | Three attempts, then failure, and no partial install left behind |
| Absent permissions | No runtime token, or a volume mounted read-only where a write is attempted, fails with a message naming the path |
| Empty repository | A repository with no commits and a repository with no `main` branch both produce a usable workspace or a clear failure |
