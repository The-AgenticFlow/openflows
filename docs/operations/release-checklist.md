# Release checklist and rollback

A published OpenFlows release cannot be changed: GitHub makes release files immutable once the draft is published. Everything that can go wrong with a release therefore has to be caught while it is still a draft, and anything that slips through is fixed by steering users away from it, never by editing it. This page lists the gates a release passes, what the operator checks by hand, and how to roll back.

## How a release is made

1. `develop` is merged into `main`. `release.yml` runs `release-plz`, which opens a `release-plz-*` PR with the next version and changelog.
2. Merging that PR makes `release.yml` create the tag `openflows-X.Y.Z` and a **draft** GitHub Release, then dispatch `release-assets.yml` and `docker-publish.yml`.
3. `release-assets.yml` builds the five archives, runs the checks below, attaches the files, waits for both images, checks them, and only then publishes the draft.
4. `docker-publish.yml` pushes `ghcr.io/the-agenticflow/openflows:X.Y.Z` and records its digest under **Docker images** in the release notes (`scripts/record-image-digest.sh`).

## Automatic gates

Every gate below must pass or the release stays a draft. A failed run can be retried with `gh workflow run release-assets.yml -f version=X.Y.Z`.

| Gate | Job | Fails when |
| --- | --- | --- |
| Archive on its own machine | `Verify (<target>)`, five runners | Checksum wrong, programs missing, wrong CPU, glibc newer than 2.17, `openflows --version` fails or prints another version, `openflows-doctor` or `openflows-harness` do not start |
| Setup kit | `Attach assets and publish` | Checksum or `RELEASE_VERSION` wrong, a file `setup.sh` reads or runs is missing, a script it runs is not executable, a Compose file or `./deploy` path is missing, a script does not parse |
| Uploaded files | `Attach assets and publish` | A file on the draft differs from the checked one |
| Images | `Attach assets and publish` | `openflows` or `openflows-manager` is missing, lacks `linux/amd64`, or is labelled with another version |

The checks are `scripts/verify-release.sh`; their tests are `tests/ci/verify-release-test.sh`.

## Manual checklist

Before merging `develop` into `main`:

- [ ] `OpenFlows Merge Gate` is green on the `develop` commit being released.
- [ ] No open PR is meant for this release.
- [ ] Changes to the setup kit (`scripts/setup*.sh`, `scripts/prod.sh`, `docker-compose*.yml`, `deploy/local`) are matched by the kit list in `release-assets.yml`.

While the draft is open:

- [ ] `release-assets.yml` for the version is green, including every `Verify` job.
- [ ] The draft has the five archives, the setup kit, and a `.sha256` for each.
- [ ] The release notes list both image digests under **Docker images**.

After publishing:

- [ ] On a clean machine, `./scripts/setup.sh owner/repo --binary` installs the release and `openflows-doctor` passes.
- [ ] `--compose` does the same with the published image.
- [ ] If the Manager runs with `OPENFLOWS_MANAGER_METRICS=true`, error rates on `/api/v1/local/github-credential` stay at their usual level (see [Metrics](#metrics)).

## Rollback

A rollback points users back at the previous release. Nothing is deleted: the bad release stays published, so anyone who must keep it can.

### Stop new installs from picking the bad release

`setup.sh` installs the version in `OPENFLOWS_VERSION`, else the setup kit's own version, else the release GitHub calls **latest** (`latest_release_version` in `scripts/setup-release.sh`). Marking the bad release as a prerelease takes it out of **latest**; making the previous release latest again sends new installs there:

```bash
gh release edit openflows-X.Y.Z --repo The-AgenticFlow/openflows --prerelease
gh release edit openflows-PREVIOUS --repo The-AgenticFlow/openflows --latest
```

A setup kit downloaded from the bad release still carries its own version. Tell those users to set `OPENFLOWS_VERSION`.

### Move an existing install back

Users roll back the same way they upgrade:

1. Set `OPENFLOWS_VERSION=PREVIOUS` in `.env`.
2. Clear `OPENFLOWS_IMAGE_DIGEST` (Compose installs).
3. Run `./scripts/setup.sh` again.

Setup downloads and verifies the previous programs, pushes their templates and offers to update the workspaces. Until every workspace runs the previous templates, setup exits with status 3 and offers the rest again on each run. Redis and Coder data are kept. The program swap, including rollback after an upgrade, is tested by `tests/ci/setup-binary-test.sh`. The full stack round trip is tracked in #528.

### Fix forward

Publish the fix as a new version. Once it passes the gates above, the release workflow marks it latest, and users who pinned the previous version can unpin.

## Metrics

The Manager can serve request counters in the Prometheus text format. They are off by default:

```bash
OPENFLOWS_MANAGER_METRICS=true
```

With the switch off, `GET /metrics` answers 503 and exposes nothing. With it on, the endpoint has no authentication, so serve it only on an internal network.

| Metric | Labels | Reading |
| --- | --- | --- |
| `openflows_manager_build_info` | `version` | Which Manager version is running |
| `openflows_manager_http_requests_total` | `method`, `route`, `status` | Requests answered per route template and status |
| `openflows_manager_http_request_duration_seconds_sum` / `_count` | `method`, `route` | Time spent answering, per route |

Labels hold only the HTTP method, the route template the Manager matched (`/api/v1/operations/{id}`, never the real id) and the status code. Request paths that match no route share the label `unmatched`, and unknown methods are counted as `OTHER`, so no user, repository, tenant, token or query value can reach the output.

Token renewal shows up as the outcomes of `POST /api/v1/local/github-credential` (`routes/local_github.rs`, `credential`):

| Status | Meaning |
| --- | --- |
| 200 | A token was handed to a workspace |
| 401 | The workspace presented the wrong broker key |
| 404 | The user and the App cannot reach that repository, or the Manager is not in local mode |
| 503 | The broker key or GitHub client is unavailable |
| Other 5xx | The Manager could not reach GitHub or its own store |

A rising share of non-200 answers after a release is the signal to roll back. These meanings follow the handler as it stands on `develop`; recheck them when that handler changes.
