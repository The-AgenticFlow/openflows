# Hosted template releases

Date: 2026-10-13. Status: implementation.

The Manager resolves template releases from an operator-configured source, so
production deployments do not need template files copied beside the binary or
bundled into every image. The source is operator configuration, never
customer-supplied.

## Sources

- **Local** (default): packages the checked-in `crates/coder-client/templates`
  directory. Used for development and tests; requires no network.
- **Hosted**: resolves a pinned, immutable **GitHub release** and verifies the
  manifest and every role archive digest before accepting anything.

## Hosted source

GitHub Releases is used because a release tag groups the manifest and one
archive per role as immutable assets, matching the existing manifest layout.
The release is pinned to a tag (never a mutable branch or `latest`) plus the
pinned manifest SHA-256, which together form the immutable release identity.

### Release layout

Each release under the pinned repository carries these assets:

| Asset name | Purpose |
|---|---|
| `manifest.json` | The typed release manifest (the release contract). |
| `openflows-nexus.tar.gz`, `openflows-forge.tar.gz`, `openflows-sentinel.tar.gz`, `openflows-vessel.tar.gz` | Required role archives. |
| `openflows-lore.tar.gz` | Optional; absent means not required. |

`manifest.json` uses the existing schema (`schema_version`, `release`,
`required_roles`, `roles`). Its canonical JSON SHA-256 is the release identity;
the operator pins that digest in configuration.

### Verification

A release is rejected (and the organization never becomes ready) when any of
these fail: the manifest digest does not match the pin; a role archive digest
does not match its manifest entry; a required role or archive asset is missing;
an archive path is unsafe (traversal/absolute), contains links, or has
unsupported entries; a download exceeds the size bound; a request times out or
the remote fails; or the release metadata is malformed. Failed downloads and
digest mismatches are never cached or imported.

## Idempotency and caching

The release identity is the manifest digest; importing the same digest returns
the existing release. Verified artifacts are cached on disk under
`OPENFLOWS_RELEASE_CACHE_DIR` (default `.openflows/release-cache`), keyed by the
digest, so restarts and retries resolve the same reference without re-download.
The cache is bounded by entry count and total bytes (oldest evicted first) and
stores only verified artifacts.

## Persistence

The existing release tables store all required metadata, so no migration is
needed: `template_releases.version`, `template_releases.manifest_sha256`,
`template_releases.artifact_uri` (the immutable reference, e.g.
`github://owner/repo@tag`), and `template_release_roles`.

## Configuration

Settings are centralized and typed in `crates/openflows-manager/src/config.rs`.

| Variable | Default | Notes |
|---|---|---|
| `OPENFLOWS_RELEASE_SOURCE` | `local` | `local` or `github`. Unknown values fail startup. |
| `OPENFLOWS_GITHUB_TEMPLATE_REPOSITORY` | — | Pinned repository as `owner/repo` (hosted). |
| `OPENFLOWS_GITHUB_TEMPLATE_TAG` | — | Immutable release tag (hosted). `latest` and branch/ref names rejected. |
| `OPENFLOWS_TEMPLATE_MANIFEST_SHA256` | — | Pinned manifest digest (hosted); the release identity. |
| `OPENFLOWS_GITHUB_TEMPLATE_TOKEN_REF` | — | Optional secret-provider reference for a private template repository. |
| `OPENFLOWS_GITHUB_API_BASE` | `https://api.github.com` | GitHub API base. |
| `OPENFLOWS_RELEASE_MAX_DOWNLOAD_BYTES` | `268435456` (256 MiB) | Per-artifact download bound. |
| `OPENFLOWS_RELEASE_TIMEOUT_SECS` | `30` | Per-request download timeout. |
| `OPENFLOWS_RELEASE_CACHE_DIR` | `.openflows/release-cache` | Verified-artifact cache directory. |

In hosted mode with a `github` source, repository, tag, and manifest digest are
required; a missing value is a startup failure, never a silent fallback.
When configured, the template token is used only for release metadata and
asset downloads; it is never persisted in release records or cache entries.

## Deployment

### Binary

Run the Manager with the hosted source configured and no template files beside
the binary. The Manager downloads, verifies, caches, and publishes the pinned
release at startup and per organization.

```sh
export OPENFLOWS_RELEASE_SOURCE=github
export OPENFLOWS_GITHUB_TEMPLATE_REPOSITORY=The-AgenticFlow/openflows-templates
export OPENFLOWS_GITHUB_TEMPLATE_TAG=2026.10.0
export OPENFLOWS_TEMPLATE_MANIFEST_SHA256=<64-char digest>
# plus the usual manager settings (OPENFLOWS_MODE=hosted, database, secrets, Coder)
openflows-manager
```

### Docker

Configure the hosted source via environment variables and provide a writable
cache volume so verified releases persist across restarts; the image does not
need template directories baked in.

```yaml
services:
  manager:
    image: openflows-manager:latest
    environment:
      OPENFLOWS_RELEASE_SOURCE: github
      OPENFLOWS_GITHUB_TEMPLATE_REPOSITORY: The-AgenticFlow/openflows-templates
      OPENFLOWS_GITHUB_TEMPLATE_TAG: "2026.10.0"
      OPENFLOWS_TEMPLATE_MANIFEST_SHA256: "<64-char digest>"
      OPENFLOWS_RELEASE_CACHE_DIR: /var/lib/openflows/release-cache
      # ... other manager settings
    volumes:
      - release-cache:/var/lib/openflows/release-cache
```

### Local development

Local mode (the default) needs no network or GitHub configuration.

```sh
OPENFLOWS_MODE=local cargo run -p openflows-manager
```

## Publishing a template release

1. Package the role directories deterministically and compute each archive
   SHA-256.
2. Build `manifest.json` and compute its canonical JSON SHA-256 (the release
   identity).
3. Create a GitHub release with a stable, immutable tag, attaching
   `manifest.json` and one archive per role.
4. Record the tag and manifest digest in the Manager configuration.

Released archives must pass the same validation the Manager applies (no links,
no unsafe paths, only regular files/directories, no `registry.json`).

## Security

- Source, repository, tag, and digest are operator configuration only.
  Customer-facing promotion still requires the platform-operator approval
  service.
- Downloads are size-bounded and time-boxed, with clear errors on failure.
- A failed download or digest mismatch never makes an organization ready.
