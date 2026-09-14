# Releasing OpenFlows

This document describes the release strategy for the `openflows` package — the
main binary (`openflows`) plus the bundled helper binaries
(`openflows-doctor`, `openflows-harness`), all built from that single release.

Releases are driven by **release-plz** on a **`develop`-as-default** branching
model, with **versions computed automatically from Conventional Commits**. There
is **no** npm, Homebrew, Docker/GHCR, or crates.io publishing; the release pulls
cross-platform binary tarballs onto a GitHub Release only.

The orchestration specs, plugins, hooks, and skills live in a **separate
repository** (not shipped in the OpenFlows release).

## Branching model

```
feature/* ──PR──▶ develop          (default branch, integration)
develop ──PR merge──▶ main          (release-only; guard enforces develop-only)
                     │  push to main → release-plz release-pr
                     ▼
        release-plz-* PR (version bump + CHANGELOG) ──▶ main
                     │  merge of that PR → release-plz release
                     ▼
        tag openflows-X.Y.Z + single GitHub Release (no `v`)
                     ▼
        release-assets → attach openflows-<X.Y.Z>-<target>.tar.gz
                         (bundles openflows, openflows-doctor, openflows-harness)
```

- `develop` is the **default branch** and the integration point. All feature
  work lands here via PR (CI + review required).
- `main` is **release-only**. Only `develop` (and release-plz's own
  `release-plz-*` version-bump PRs) may be merged into `main`. A guard workflow
  (`release-branch.yml`) rejects any other PR to `main`.
- release-plz is the sole publisher; the `release-assets` workflow attaches
  binaries.

## How versions are provided (automatic)

You never type a version. release-plz derives the next version from the
Conventional Commits merged since the last tag:

| Commit type / footer             | Version bump |
|----------------------------------|--------------|
| `BREAKING CHANGE:` (any type)    | **major**    |
| `feat:`                          | **minor**    |
| `fix:`, `docs:`, `chore:`, …     | **patch**    |

`release-plz` opens a `release-plz-*` pull request to `main` that bumps the
released package's manifest version and regenerates the CHANGELOG. Merging that
PR "locks in" the new version and triggers the tag + GitHub Release.

> The very first release (no tags yet in the repo — the old tag history was
> deleted for a clean start) is treated as an **initial release** and ships at
> the current version in the manifest (`1.2.0`). After that, all releases are
> computed from conventional commits.

## Day-to-day workflow

1. Branch from `develop` (`feature/*`) and open a PR into `develop`.
2. CI runs on `develop`; merge once green.
3. Write Conventional Commit messages (`feat:`, `fix:`, `docs:`, …) — a
   **commitlint** guard enforces this on every PR (title and commit messages).

## Cutting a release

1. Open a PR from `develop` into `main` and merge it.

   - The release-branch guard requires the PR to come from `develop` (or a
     `release-plz-*` branch).
   - Merging `develop` → `main` is the single action that triggers the release.

2. On the push to `main`, **release-plz `release-pr`** opens (or updates) a
   `release-plz-*` PR to `main` bumping the version + CHANGELOG. Review and
   merge it so the new version is locked in on `main`. (If no release PR
   appears, run the `Release → release-plz (release-pr)` workflow manually.)

3. Merging that `release-plz-*` PR pushes `main` again, which triggers
   **release-plz `release`** — it creates one tag and GitHub Release for the
   `openflows` package:

   - `openflows-X.Y.Z` (release **openflows**).

   Tag/release name carries no `v` prefix.

4. The tag push triggers **release-assets**, which builds and attaches the
   cross-platform tarballs:

   - `openflows-<X.Y.Z>-<target>.tar.gz` + `.sha256`
     for x86_64/aarch64 Linux-GNU (zigbuild), Linux-musl, and macOS, each
     containing the `openflows`, `openflows-doctor`, and `openflows-harness`
     binaries.

   Asset names are fixed/predictable, so re-runs overwrite rather than error.

## Commit message convention

release-plz computes the version and builds the CHANGELOG from commit subjects
between releases, so **every merge message must be a Conventional Commit** —
`feat:`, `fix:`, `docs:`, `chore:`, `refactor:`, `perf:`, `test:` — with
`BREAKING CHANGE:` in the footer for breaking changes. The commitlint guard
enforces this on every PR.

## Notes

- Single release: only `openflows` is released in `release-plz.toml`, with tag
  `openflows-X.Y.Z` (no `v` prefix). The `openflows-doctor` and
  `openflows-harness` binaries are built from the same release and bundled into
  the `openflows` tarball; they are not released as separate packages.
- No crates.io publishing is configured (these are application binaries);
  `release-plz.toml` uses `git_only = true` and `publish = false`.
- `orchestration/` is not bundled or shipped with the release; it is maintained
  in a separate repository.
