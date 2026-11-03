//! Release-source abstraction.
//!
//! A template release can be resolved from the operator-configured local
//! filesystem (development and tests) or from a pinned, immutable hosted
//! GitHub release. Both produce a [`ResolvedRelease`] of validated artifacts.
//!
//! Hosted resolution verifies the complete manifest digest and every role
//! archive digest before accepting a release, bounds download sizes, applies
//! per-request timeouts, rejects unsafe archive contents, and caches verified
//! artifacts on disk so Manager restarts and retries do not require
//! unnecessary downloads. The hosted source is operator configuration only:
//! customer organizations can never supply arbitrary template URLs.

use crate::config::HostedReleaseConfig;
use crate::error::ManagerError;
use crate::templates::packaging::{
    sha256_hex, validate_archive, verify_archive_digest, Role, TemplateManifest,
};
use crate::templates::service::ReleaseArtifacts;
use futures_util::StreamExt;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::Duration;
use url::Url;

use super::release_provider::{ReleaseProvider, ReleaseProviderError};

/// A resolved, fully verified release from an operator-configured source.
#[derive(Clone, Debug)]
pub struct ResolvedRelease {
    /// The validated manifest and deterministic role archives.
    pub artifacts: ReleaseArtifacts,
    /// The immutable source reference (e.g. `github://owner/repo@tag`).
    pub source_reference: String,
    /// The manifest SHA-256 digest; the immutable release identity.
    pub manifest_digest: String,
}

/// The operator-configured release source.
#[derive(Clone)]
pub enum ReleaseSource {
    /// Local filesystem source (development and tests; no network required).
    Local(LocalReleaseSource),
    /// Hosted source backed by immutable GitHub Releases.
    Hosted(HostedReleaseSource),
}

impl ReleaseSource {
    /// Build a release source from centralized typed configuration.
    pub fn from_config(
        config: &crate::config::ReleaseSourceConfig,
        github_token: Option<String>,
    ) -> Result<Self, ManagerError> {
        match config {
            crate::config::ReleaseSourceConfig::Local => Ok(ReleaseSource::Local(
                LocalReleaseSource::from_env().map_err(ManagerError::from)?,
            )),
            crate::config::ReleaseSourceConfig::Github(hosted) => Ok(ReleaseSource::Hosted(
                HostedReleaseSource::new(hosted.clone(), github_token),
            )),
        }
    }

    /// Resolve and verify the configured release.
    pub async fn resolve(&self, cache: &ReleaseCache) -> Result<ResolvedRelease, ManagerError> {
        match self {
            ReleaseSource::Local(source) => source.resolve(),
            ReleaseSource::Hosted(source) => source.resolve(cache).await,
        }
    }
}

/// The local filesystem release source. Preserves the existing development
/// and test behavior; requires no network access.
#[derive(Clone)]
pub struct LocalReleaseSource {
    provider: ReleaseProvider,
}

impl LocalReleaseSource {
    /// Build from environment configuration (existing local behavior).
    pub fn from_env() -> Result<Self, ReleaseProviderError> {
        Ok(LocalReleaseSource {
            provider: ReleaseProvider::from_env()?,
        })
    }

    /// Build from explicit paths (tests/operator tooling).
    pub fn new(
        source_dir: PathBuf,
        manifest_path: Option<PathBuf>,
        manifest_sha256: Option<String>,
        release_name: String,
    ) -> Self {
        LocalReleaseSource {
            provider: ReleaseProvider::new(
                source_dir,
                manifest_path,
                manifest_sha256,
                release_name,
            ),
        }
    }

    /// Resolve the local release, packaging and validating the role archives.
    pub fn resolve(&self) -> Result<ResolvedRelease, ManagerError> {
        let artifacts = self.provider.load().map_err(ManagerError::from)?;
        let manifest_digest = artifacts.manifest.manifest_sha256()?;
        let source_reference = format!("local://{}", self.provider.source_dir().display());
        Ok(ResolvedRelease {
            artifacts,
            source_reference,
            manifest_digest,
        })
    }
}

/// A hosted release source backed by GitHub Releases.
#[derive(Clone)]
pub struct HostedReleaseSource {
    config: HostedReleaseConfig,
    fetcher: std::sync::Arc<dyn ReleaseFetcher>,
}

impl HostedReleaseSource {
    /// Build a hosted source from typed configuration, using the real
    /// `reqwest`-backed fetcher.
    pub fn new(config: HostedReleaseConfig, github_token: Option<String>) -> Self {
        HostedReleaseSource {
            config,
            fetcher: std::sync::Arc::new(ReqwestReleaseFetcher::new(github_token)),
        }
    }

    /// Build a hosted source with an explicit fetcher (tests).
    pub fn with_fetcher(
        config: HostedReleaseConfig,
        fetcher: std::sync::Arc<dyn ReleaseFetcher>,
    ) -> Self {
        HostedReleaseSource { config, fetcher }
    }

    /// Resolve and verify the pinned release, consulting the on-disk cache
    /// first so repeated resolution of the same immutable reference is
    /// idempotent and does not re-download.
    pub async fn resolve(&self, cache: &ReleaseCache) -> Result<ResolvedRelease, ManagerError> {
        let reference = format!("github://{}@{}", self.config.repository, self.config.tag);

        if let Some(cached) = cache
            .load(&self.config.manifest_sha256)
            .map_err(|e| ManagerError::Config(format!("failed to read release cache: {e}")))?
        {
            let digest = sha256_hex(&cached.manifest_json);
            if digest == self.config.manifest_sha256 {
                let manifest = TemplateManifest::parse(&cached.manifest_json).map_err(|e| {
                    ManagerError::Config(format!("cached manifest is invalid: {e}"))
                })?;
                if let Ok(artifacts) = build_artifacts_from_cache(&manifest, cached.archives) {
                    let manifest_digest = manifest.manifest_sha256()?;
                    return Ok(ResolvedRelease {
                        artifacts,
                        source_reference: reference,
                        manifest_digest,
                    });
                }
            }
        }

        let resolved = self.fetch_and_verify(&reference).await?;

        cache
            .store(&resolved.manifest_digest, &resolved.artifacts, &reference)
            .map_err(|e| ManagerError::Config(format!("failed to write release cache: {e}")))?;

        Ok(resolved)
    }

    async fn fetch_and_verify(&self, reference: &str) -> Result<ResolvedRelease, ManagerError> {
        let info_url = github_release_metadata_url(&self.config)?;
        let info_bytes = self
            .fetcher
            .get_bytes(&info_url, self.config.max_download_bytes, self.timeout())
            .await
            .map_err(|e| {
                ManagerError::Config(format!("cannot reach hosted release source: {e}"))
            })?;
        let info: serde_json::Value = serde_json::from_slice(&info_bytes).map_err(|_| {
            ManagerError::Config("hosted release source returned invalid release metadata".into())
        })?;

        // Map asset name -> browser_download_url from the release metadata.
        let assets = info
            .get("assets")
            .and_then(|v| v.as_array())
            .ok_or_else(|| ManagerError::Config("hosted release metadata has no assets".into()))?;
        let mut asset_urls: BTreeMap<String, String> = BTreeMap::new();
        for asset in assets {
            let name = asset.get("name").and_then(|v| v.as_str());
            let url = asset.get("browser_download_url").and_then(|v| v.as_str());
            if let (Some(name), Some(url)) = (name, url) {
                asset_urls.insert(name.to_string(), url.to_string());
            }
        }

        let manifest_asset = "manifest.json";
        let manifest_url = asset_urls.get(manifest_asset).ok_or_else(|| {
            ManagerError::Config(format!(
                "hosted release {reference} is missing the manifest asset '{manifest_asset}'"
            ))
        })?;
        let manifest_bytes = self
            .fetcher
            .get_bytes(manifest_url, self.config.max_download_bytes, self.timeout())
            .await
            .map_err(|e| ManagerError::Config(format!("failed to download manifest: {e}")))?;

        let manifest = TemplateManifest::parse(&manifest_bytes)
            .map_err(|e| ManagerError::Config(format!("hosted manifest is invalid: {e}")))?;
        // The release identity is the canonical manifest digest. This keeps
        // downloaded bytes, cache entries, and database records consistent.
        let actual_manifest_digest = manifest.manifest_sha256()?;
        if actual_manifest_digest != self.config.manifest_sha256 {
            return Err(ManagerError::Config(format!(
                "manifest digest mismatch: expected {}, got {}",
                self.config.manifest_sha256, actual_manifest_digest
            )));
        }

        let mut archives = BTreeMap::new();
        for (role, entry) in &manifest.roles {
            let asset_url = asset_urls.get(&entry.archive).ok_or_else(|| {
                ManagerError::Config(format!(
                    "hosted release {reference} is missing the archive asset '{}' for role '{}'",
                    entry.archive,
                    role.as_str()
                ))
            })?;
            let archive = self
                .fetcher
                .get_bytes(asset_url, self.config.max_download_bytes, self.timeout())
                .await
                .map_err(|e| {
                    ManagerError::Config(format!(
                        "failed to download archive for role '{}': {e}",
                        role.as_str()
                    ))
                })?;
            verify_archive_digest(&archive, &entry.sha256).map_err(|e| {
                ManagerError::Config(format!(
                    "role '{}' archive does not match manifest digest: {e}",
                    role.as_str()
                ))
            })?;
            validate_archive(&archive).map_err(|e| {
                ManagerError::Config(format!("role '{}' archive is unsafe: {e}", role.as_str()))
            })?;
            archives.insert(*role, archive);
        }

        for role in &manifest.required_roles {
            if !archives.contains_key(role) {
                return Err(ManagerError::Config(format!(
                    "hosted release {reference} is missing the required role '{}'",
                    role.as_str()
                )));
            }
        }

        Ok(ResolvedRelease {
            artifacts: ReleaseArtifacts { manifest, archives },
            source_reference: reference.to_string(),
            manifest_digest: actual_manifest_digest,
        })
    }

    fn timeout(&self) -> Duration {
        Duration::from_secs(self.config.timeout_secs)
    }
}

fn github_release_metadata_url(config: &HostedReleaseConfig) -> Result<String, ManagerError> {
    let (owner, repository) = config.repository.split_once('/').ok_or_else(|| {
        ManagerError::Config("GitHub template repository must be owner/repository".into())
    })?;
    let mut url = Url::parse(&config.api_base)
        .map_err(|e| ManagerError::Config(format!("invalid GitHub API base URL: {e}")))?;
    {
        let mut segments = url.path_segments_mut().map_err(|_| {
            ManagerError::Config("GitHub API base URL cannot be a cannot-be-a-base URL".into())
        })?;
        segments.extend(["repos", owner, repository, "releases", "tags", &config.tag]);
    }
    Ok(url.into())
}

/// Reassemble validated artifacts from a cached manifest and archive bytes.
fn build_artifacts_from_cache(
    manifest: &TemplateManifest,
    archives: BTreeMap<Role, Vec<u8>>,
) -> Result<ReleaseArtifacts, ManagerError> {
    for (role, entry) in &manifest.roles {
        let archive = archives.get(role).ok_or_else(|| {
            ManagerError::Config(format!(
                "cached release is missing archive for role '{}'",
                role.as_str()
            ))
        })?;
        verify_archive_digest(archive, &entry.sha256)?;
        validate_archive(archive)?;
    }
    Ok(ReleaseArtifacts {
        manifest: manifest.clone(),
        archives,
    })
}

// ---------------------------------------------------------------------------
// Fetcher abstraction (network layer).
// ---------------------------------------------------------------------------

/// An error from a hosted download attempt, with a clear, operator-facing
/// message.
#[derive(Debug, Clone)]
pub struct ReleaseFetchError(pub String);

impl std::fmt::Display for ReleaseFetchError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for ReleaseFetchError {}

/// Fetches a single URL into memory with a size bound and a timeout.
#[async_trait::async_trait]
pub trait ReleaseFetcher: Send + Sync {
    /// Fetch `url`, returning the body if it is smaller than `max_bytes` and
    /// arrives within `timeout`. Oversized bodies and remote failures are
    /// errors.
    async fn get_bytes(
        &self,
        url: &str,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, ReleaseFetchError>;
}

/// The real `reqwest`-backed fetcher.
pub struct ReqwestReleaseFetcher {
    client: reqwest::Client,
    github_token: Option<String>,
}

impl ReqwestReleaseFetcher {
    fn new(github_token: Option<String>) -> Self {
        ReqwestReleaseFetcher {
            client: reqwest::Client::new(),
            github_token,
        }
    }
}

#[async_trait::async_trait]
impl ReleaseFetcher for ReqwestReleaseFetcher {
    async fn get_bytes(
        &self,
        url: &str,
        max_bytes: usize,
        timeout: Duration,
    ) -> Result<Vec<u8>, ReleaseFetchError> {
        let mut request = self
            .client
            .get(url)
            .timeout(timeout)
            .header(reqwest::header::USER_AGENT, "openflows-manager");
        if let Some(token) = &self.github_token {
            request = request.bearer_auth(token);
        }
        let response = request
            .send()
            .await
            .map_err(|e| ReleaseFetchError(format!("request to {url} failed: {e}")))?;

        let status = response.status();
        if !status.is_success() {
            return Err(ReleaseFetchError(format!(
                "request to {url} returned HTTP {status}"
            )));
        }

        if let Some(len) = response.content_length() {
            if len > max_bytes as u64 {
                return Err(ReleaseFetchError(format!(
                    "download from {url} exceeds the {max_bytes}-byte limit"
                )));
            }
        }

        let mut body: Vec<u8> = Vec::new();
        let mut stream = response.bytes_stream();
        while let Some(chunk) = stream.next().await {
            let chunk =
                chunk.map_err(|e| ReleaseFetchError(format!("download from {url} failed: {e}")))?;
            if body.len() + chunk.len() > max_bytes {
                return Err(ReleaseFetchError(format!(
                    "download from {url} exceeds the {max_bytes}-byte limit"
                )));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }
}

// ---------------------------------------------------------------------------
// Bounded on-disk verified-artifact cache.
// ---------------------------------------------------------------------------

/// A bounded on-disk cache of verified releases, keyed by manifest digest.
///
/// Storing only *verified* artifacts (digest checked before writing) means a
/// Manager restart or a retry resolves the same immutable reference without a
/// network round-trip. The cache is bounded by entry count and total bytes;
/// the oldest entries are evicted first.
#[derive(Clone)]
pub struct ReleaseCache {
    dir: PathBuf,
    max_entries: usize,
    max_total_bytes: usize,
}

const CACHE_ENTRY_MARKER: &str = ".openflows-release-entry";
const CACHE_TEMP_PREFIX: &str = ".tmp-";
const CACHE_TEMP_TTL: Duration = Duration::from_secs(24 * 60 * 60);

impl ReleaseCache {
    /// Build a cache rooted at `dir` with the given bounds.
    pub fn new(dir: PathBuf, max_entries: usize, max_total_bytes: usize) -> Self {
        ReleaseCache {
            dir,
            max_entries,
            max_total_bytes,
        }
    }

    /// Build a cache from a hosted config's settings.
    pub fn from_config(config: &HostedReleaseConfig) -> Self {
        ReleaseCache::new(
            config.cache_dir.clone(),
            crate::config::DEFAULT_RELEASE_CACHE_MAX_ENTRIES,
            config
                .max_download_bytes
                .saturating_mul(crate::config::DEFAULT_RELEASE_CACHE_MAX_ENTRIES)
                .saturating_mul(Role::ALL.len() + 1),
        )
    }

    /// Load a complete verified release entry for `digest`, if present.
    pub fn load(&self, digest: &str) -> std::io::Result<Option<CachedRelease>> {
        self.cleanup_temporary_entries()?;
        let entry_dir = self.entry_dir(digest);
        if !entry_dir.join(CACHE_ENTRY_MARKER).is_file() {
            return Ok(None);
        }
        let manifest_path = entry_dir.join("manifest.json");
        if !manifest_path.is_file() {
            return Ok(None);
        }
        let manifest_json = match std::fs::read(&manifest_path) {
            Ok(bytes) => bytes,
            Err(_) => return Ok(None),
        };
        let Ok(manifest) = TemplateManifest::parse(&manifest_json) else {
            return Ok(None);
        };
        if manifest
            .to_canonical_json()
            .ok()
            .map(|bytes| sha256_hex(&bytes))
            .as_deref()
            != Some(digest)
        {
            return Ok(None);
        }
        let mut archives = BTreeMap::new();
        for role in manifest.roles.keys() {
            let archive_path = entry_dir.join(role.archive_name());
            let Ok(bytes) = std::fs::read(&archive_path) else {
                return Ok(None);
            };
            archives.insert(*role, bytes);
        }
        if build_artifacts_from_cache(&manifest, archives.clone()).is_err() {
            return Ok(None);
        }
        Ok(Some(CachedRelease {
            manifest_json,
            archives,
        }))
    }

    /// Store a verified release under its manifest digest, evicting oldest
    /// entries when the cache exceeds its bounds.
    pub fn store(
        &self,
        digest: &str,
        artifacts: &ReleaseArtifacts,
        _source_reference: &str,
    ) -> std::io::Result<()> {
        if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cache key must be a SHA-256 hex digest",
            ));
        }
        let manifest_bytes = artifacts
            .manifest
            .to_canonical_json()
            .map_err(std::io::Error::other)?;
        if sha256_hex(&manifest_bytes) != digest {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "cache key does not match manifest digest",
            ));
        }
        let entry_bytes = manifest_bytes.len()
            + artifacts.archives.values().map(Vec::len).sum::<usize>()
            + CACHE_ENTRY_MARKER.len();
        if entry_bytes > self.max_total_bytes {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "release exceeds the configured cache size",
            ));
        }

        std::fs::create_dir_all(self.entries_dir())?;
        // Serialize publication, repair, and eviction across Manager processes.
        // The OS releases this lock if a writer crashes.
        let lock = std::fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(self.dir.join(".writer.lock"))?;
        lock.lock()?;
        let temp_dir = self.entries_dir().join(format!(
            ".tmp-{digest}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        ));
        std::fs::create_dir(&temp_dir)?;
        let result = self.store_complete_entry(&temp_dir, &manifest_bytes, artifacts);
        if let Err(error) = result {
            let _ = std::fs::remove_dir_all(&temp_dir);
            return Err(error);
        }
        let entry_dir = self.entry_dir(digest);
        match std::fs::rename(&temp_dir, &entry_dir) {
            Ok(()) => {}
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::AlreadyExists | std::io::ErrorKind::DirectoryNotEmpty
                ) =>
            {
                if self.load(digest)?.is_some() {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                } else if entry_dir.join(CACHE_ENTRY_MARKER).is_file() {
                    std::fs::remove_dir_all(&entry_dir)?;
                    std::fs::rename(&temp_dir, &entry_dir)?;
                } else {
                    let _ = std::fs::remove_dir_all(&temp_dir);
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::AlreadyExists,
                        "cache entry path is occupied by an unmanaged directory",
                    ));
                }
            }
            Err(error) => {
                let _ = std::fs::remove_dir_all(&temp_dir);
                return Err(error);
            }
        }
        self.enforce_bounds(digest)
    }

    fn store_complete_entry(
        &self,
        entry_dir: &Path,
        manifest_bytes: &[u8],
        artifacts: &ReleaseArtifacts,
    ) -> std::io::Result<()> {
        std::fs::write(entry_dir.join("manifest.json"), manifest_bytes)?;
        for (role, archive) in &artifacts.archives {
            std::fs::write(entry_dir.join(role.archive_name()), archive)?;
        }
        std::fs::write(entry_dir.join(CACHE_ENTRY_MARKER), b"openflows\n")?;
        Ok(())
    }

    /// The root cache directory.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    fn entry_dir(&self, digest: &str) -> PathBuf {
        self.entries_dir().join(digest)
    }

    fn entries_dir(&self) -> PathBuf {
        self.dir.join("entries")
    }

    fn enforce_bounds(&self, keep: &str) -> std::io::Result<()> {
        let mut entries: Vec<(std::time::SystemTime, PathBuf)> = Vec::new();
        let entries_dir = self.entries_dir();
        if !entries_dir.is_dir() {
            return Ok(());
        }
        for entry in std::fs::read_dir(entries_dir)? {
            let entry = entry?;
            if entry
                .file_name()
                .to_string_lossy()
                .starts_with(CACHE_TEMP_PREFIX)
            {
                continue;
            }
            if !entry.path().is_dir() || !entry.path().join(CACHE_ENTRY_MARKER).is_file() {
                continue;
            }
            let modified = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            entries.push((modified, entry.path()));
        }
        entries.sort_by_key(|(t, _)| *t);

        let mut total: u64 = 0;
        for (_, path) in &entries {
            total += dir_size(path);
        }

        while entries.len() > self.max_entries || total > self.max_total_bytes as u64 {
            let Some((_, oldest)) = entries.first().cloned() else {
                break;
            };
            if oldest == self.entry_dir(keep) {
                if entries.len() == 1 {
                    break;
                }
                entries.remove(0);
                continue;
            }
            total = total.saturating_sub(dir_size(&oldest));
            if let Err(error) = std::fs::remove_dir_all(&oldest) {
                if error.kind() != std::io::ErrorKind::NotFound {
                    return Err(error);
                }
            }
            entries.remove(0);
        }
        Ok(())
    }

    fn cleanup_temporary_entries(&self) -> std::io::Result<()> {
        let entries_dir = self.entries_dir();
        if !entries_dir.is_dir() {
            return Ok(());
        }
        let now = std::time::SystemTime::now();
        for entry in std::fs::read_dir(entries_dir)? {
            let entry = entry?;
            if !entry
                .file_name()
                .to_string_lossy()
                .starts_with(CACHE_TEMP_PREFIX)
            {
                continue;
            }
            let stale = entry
                .metadata()
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| now.duration_since(modified).ok())
                .is_some_and(|age| age > CACHE_TEMP_TTL);
            if stale {
                let _ = std::fs::remove_dir_all(entry.path());
            }
        }
        Ok(())
    }
}

/// A cached, previously verified release.
pub struct CachedRelease {
    /// The canonical manifest JSON bytes.
    pub manifest_json: Vec<u8>,
    /// role -> verified archive bytes.
    pub archives: BTreeMap<Role, Vec<u8>>,
}

/// Total size of the files directly under `dir`.
fn dir_size(dir: &Path) -> u64 {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return 0;
    };
    rd.filter_map(|e| e.ok())
        .filter_map(|e| e.metadata().ok())
        .map(|m| m.len())
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::templates::packaging::{build_deterministic_archive, RoleManifestEntry};
    use axum::{extract::Request, http::StatusCode, response::IntoResponse, routing::get, Router};
    use std::collections::BTreeMap;

    // -----------------------------------------------------------------------
    // Test doubles.
    // -----------------------------------------------------------------------

    /// A scripted fetcher: returns bytes by exact URL, or a configured error.
    /// Counts calls so idempotency tests can assert the network is not hit on
    /// a cache hit. Mirrors the real fetcher by rejecting bodies over the
    /// requested size bound.
    struct FakeFetcher {
        responses: BTreeMap<String, Result<Vec<u8>, ReleaseFetchError>>,
        calls: std::sync::atomic::AtomicUsize,
    }

    impl FakeFetcher {
        fn new(
            responses: BTreeMap<String, Result<Vec<u8>, ReleaseFetchError>>,
        ) -> std::sync::Arc<Self> {
            std::sync::Arc::new(FakeFetcher {
                responses,
                calls: std::sync::atomic::AtomicUsize::new(0),
            })
        }

        fn call_count(&self) -> usize {
            self.calls.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[async_trait::async_trait]
    impl ReleaseFetcher for FakeFetcher {
        async fn get_bytes(
            &self,
            url: &str,
            max_bytes: usize,
            _timeout: Duration,
        ) -> Result<Vec<u8>, ReleaseFetchError> {
            self.calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            let result = self.responses.get(url).cloned().unwrap_or_else(|| {
                Err(ReleaseFetchError(format!(
                    "unexpected URL requested: {url}"
                )))
            })?;
            if result.len() > max_bytes {
                return Err(ReleaseFetchError(format!(
                    "download from {url} exceeds the {max_bytes}-byte limit"
                )));
            }
            Ok(result)
        }
    }

    fn build_test_release() -> (
        ReleaseArtifacts,
        Vec<u8>,
        BTreeMap<String, Vec<u8>>,
        tempfile::TempDir,
    ) {
        let dir = tempfile::tempdir().unwrap();
        let mut archives = BTreeMap::new();
        let mut roles = BTreeMap::new();
        let mut assets = BTreeMap::new();
        for role in [Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel] {
            let role_dir = dir.path().join(format!("openflows-{}", role.as_str()));
            std::fs::create_dir_all(&role_dir).unwrap();
            std::fs::write(
                role_dir.join("main.tf"),
                format!("resource \"t\" {{ name = \"{}\" }}\n", role.as_str()),
            )
            .unwrap();
            let archive = build_deterministic_archive(&role_dir).unwrap();
            let sha256 = sha256_hex(&archive);
            assets.insert(role.archive_name(), archive.clone());
            archives.insert(role, archive);
            roles.insert(
                role,
                RoleManifestEntry {
                    role,
                    archive: role.archive_name(),
                    sha256,
                    parameter_schema_version: 1,
                    image_digest: None,
                },
            );
        }
        let manifest = TemplateManifest {
            schema_version: 1,
            release: "2026.10.0".into(),
            required_roles: vec![Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel],
            roles,
        };
        let manifest_json = manifest.to_canonical_json().unwrap();
        assets.insert("manifest.json".into(), manifest_json.clone());
        (
            ReleaseArtifacts { manifest, archives },
            manifest_json,
            assets,
            dir,
        )
    }

    fn hosted_config(
        manifest_digest: String,
        cache_dir: std::path::PathBuf,
    ) -> HostedReleaseConfig {
        HostedReleaseConfig {
            repository: "The-AgenticFlow/openflows-templates".into(),
            tag: "2026.10.0".into(),
            manifest_sha256: manifest_digest,
            github_token_ref: None,
            api_base: "https://api.example.com".into(),
            max_download_bytes: 256 * 1024 * 1024,
            timeout_secs: 30,
            cache_dir,
        }
    }

    fn asset_responses(
        info_url: &str,
        base: &str,
        assets: &BTreeMap<String, Vec<u8>>,
    ) -> BTreeMap<String, Result<Vec<u8>, ReleaseFetchError>> {
        let mut responses = BTreeMap::new();
        let mut info_assets = Vec::new();
        for (name, bytes) in assets {
            let url = format!("{base}/{name}");
            responses.insert(url.clone(), Ok(bytes.clone()));
            info_assets.push(serde_json::json!({
                "name": name,
                "browser_download_url": url,
            }));
        }
        let info = serde_json::json!({
            "assets": info_assets,
        });
        responses.insert(info_url.to_string(), Ok(serde_json::to_vec(&info).unwrap()));
        responses
    }

    fn info_url(config: &HostedReleaseConfig) -> String {
        format!(
            "{}/repos/{}/releases/tags/{}",
            config.api_base, config.repository, config.tag
        )
    }

    fn release_dir(config: &HostedReleaseConfig) -> String {
        format!("{}/download", config.api_base)
    }

    async fn spawn_http_server(app: Router) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        (format!("http://{address}"), task)
    }

    #[test]
    fn metadata_url_encodes_tag_as_a_path_segment() {
        let mut config = hosted_config("a".repeat(64), PathBuf::from("/tmp/cache"));
        config.tag = "release#2026.10.0".into();
        let url = github_release_metadata_url(&config).unwrap();
        assert!(url.ends_with("/releases/tags/release%232026.10.0"));
        assert!(Url::parse(&url).unwrap().fragment().is_none());
    }

    #[tokio::test]
    async fn real_fetcher_enforces_timeout_size_and_bearer_auth() {
        let app = Router::new()
            .route(
                "/slow",
                get(|| async {
                    axum::body::Body::from_stream(futures_util::stream::once(async {
                        tokio::time::sleep(Duration::from_millis(100)).await;
                        Ok::<_, std::io::Error>("slow")
                    }))
                }),
            )
            .route(
                "/large",
                get(|| async {
                    axum::body::Body::from_stream(futures_util::stream::iter([
                        Ok::<_, std::io::Error>(vec![b'x'; 12]),
                        Ok(vec![b'x'; 12]),
                    ]))
                }),
            )
            .route(
                "/private",
                get(|request: Request| async move {
                    if request.headers().get("authorization")
                        == Some(&axum::http::HeaderValue::from_static("Bearer secret"))
                    {
                        StatusCode::OK.into_response()
                    } else {
                        StatusCode::UNAUTHORIZED.into_response()
                    }
                }),
            );
        let (base, server) = spawn_http_server(app).await;
        let fetcher = ReqwestReleaseFetcher::new(Some("secret".into()));

        let timeout_error = fetcher
            .get_bytes(&format!("{base}/slow"), 1024, Duration::from_millis(10))
            .await
            .unwrap_err();
        assert!(timeout_error.to_string().contains("failed"));

        let size_error = fetcher
            .get_bytes(&format!("{base}/large"), 16, Duration::from_secs(1))
            .await
            .unwrap_err();
        assert!(size_error.to_string().contains("exceeds"));

        let authorized = fetcher
            .get_bytes(&format!("{base}/private"), 1024, Duration::from_secs(1))
            .await
            .unwrap();
        assert!(authorized.is_empty());
        server.abort();
    }

    // -----------------------------------------------------------------------
    // Hosted release resolution.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn hosted_resolution_succeeds_and_verifies_digests() {
        let (_artifacts, manifest_json, assets, _dir) = build_test_release();
        let digest = sha256_hex(&manifest_json);
        let cache_dir = tempfile::tempdir().unwrap();
        let config = hosted_config(digest.clone(), cache_dir.path().to_path_buf());
        let fetcher = FakeFetcher::new(asset_responses(
            &info_url(&config),
            &release_dir(&config),
            &assets,
        ));

        let source = HostedReleaseSource::with_fetcher(config.clone(), fetcher.clone());
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);
        let resolved = source.resolve(&cache).await.unwrap();

        assert_eq!(resolved.manifest_digest, digest);
        assert_eq!(
            resolved.source_reference,
            "github://The-AgenticFlow/openflows-templates@2026.10.0"
        );
        assert_eq!(resolved.artifacts.manifest.release, "2026.10.0");
        for role in resolved.artifacts.manifest.roles.keys() {
            assert!(resolved.artifacts.archives.contains_key(role));
        }
        let nexus = resolved.artifacts.archives.get(&Role::Nexus).unwrap();
        let entry = resolved.artifacts.manifest.roles.get(&Role::Nexus).unwrap();
        verify_archive_digest(nexus, &entry.sha256).unwrap();
    }

    #[tokio::test]
    async fn hosted_resolution_uses_canonical_manifest_digest_for_formatted_json() {
        let (_artifacts, manifest_json, mut assets, _dir) = build_test_release();
        let digest = sha256_hex(&manifest_json);
        let formatted = serde_json::to_vec_pretty(
            &serde_json::from_slice::<serde_json::Value>(&manifest_json).unwrap(),
        )
        .unwrap();
        assets.insert("manifest.json".into(), formatted);
        let cache_dir = tempfile::tempdir().unwrap();
        let config = hosted_config(digest.clone(), cache_dir.path().to_path_buf());
        let fetcher = FakeFetcher::new(asset_responses(
            &info_url(&config),
            &release_dir(&config),
            &assets,
        ));

        let source = HostedReleaseSource::with_fetcher(config, fetcher);
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);
        let resolved = source.resolve(&cache).await.unwrap();
        assert_eq!(resolved.manifest_digest, digest);
        assert!(cache.load(&digest).unwrap().is_some());
    }

    #[tokio::test]
    async fn hosted_manifest_digest_mismatch_is_rejected() {
        let (_artifacts, _manifest_json, assets, _dir) = build_test_release();
        let wrong_digest = "b".repeat(64);
        let cache_dir = tempfile::tempdir().unwrap();
        let config = hosted_config(wrong_digest.clone(), cache_dir.path().to_path_buf());
        let fetcher = FakeFetcher::new(asset_responses(
            &info_url(&config),
            &release_dir(&config),
            &assets,
        ));

        let source = HostedReleaseSource::with_fetcher(config.clone(), fetcher);
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);
        let err = source.resolve(&cache).await.unwrap_err();
        assert!(
            err.to_string().contains("manifest digest mismatch"),
            "unexpected error: {err}"
        );
        assert!(cache.load(&wrong_digest).unwrap().is_none());
    }

    #[tokio::test]
    async fn hosted_role_archive_digest_mismatch_is_rejected() {
        let (artifacts, manifest_json, mut assets, _dir) = build_test_release();
        let digest = sha256_hex(&manifest_json);
        let cache_dir = tempfile::tempdir().unwrap();
        let config = hosted_config(digest, cache_dir.path().to_path_buf());

        let bad_archive = {
            let role_dir = _dir.path().join("openflows-nexus");
            std::fs::write(role_dir.join("main.tf"), "corrupted\n").unwrap();
            build_deterministic_archive(&role_dir).unwrap()
        };
        assert_ne!(
            sha256_hex(&bad_archive),
            artifacts.manifest.roles[&Role::Nexus].sha256
        );
        assets.insert(Role::Nexus.archive_name(), bad_archive);

        let fetcher = FakeFetcher::new(asset_responses(
            &info_url(&config),
            &release_dir(&config),
            &assets,
        ));
        let source = HostedReleaseSource::with_fetcher(config, fetcher);
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);
        let err = source.resolve(&cache).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("archive does not match manifest digest"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn hosted_missing_required_role_asset_is_rejected() {
        let (_artifacts, manifest_json, mut assets, _dir) = build_test_release();
        let digest = sha256_hex(&manifest_json);
        let cache_dir = tempfile::tempdir().unwrap();
        let config = hosted_config(digest, cache_dir.path().to_path_buf());

        assets.remove(&Role::Nexus.archive_name());

        let fetcher = FakeFetcher::new(asset_responses(
            &info_url(&config),
            &release_dir(&config),
            &assets,
        ));
        let source = HostedReleaseSource::with_fetcher(config, fetcher);
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);
        let err = source.resolve(&cache).await.unwrap_err();
        assert!(
            err.to_string().contains("missing the archive asset"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn hosted_manifest_missing_required_role_entry_is_rejected() {
        let (_artifacts, manifest_json, mut assets, _dir) = build_test_release();
        // Rewrite the manifest to drop nexus from required roles, so parse
        // rejects it (baseline role must be required).
        let mut manifest: serde_json::Value = serde_json::from_slice(&manifest_json).unwrap();
        let required = manifest["required_roles"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|r| r.as_str() != Some("nexus"))
            .cloned()
            .collect::<Vec<_>>();
        manifest["required_roles"] = serde_json::Value::Array(required);
        let bad_manifest = serde_json::to_vec(&manifest).unwrap();
        let digest = sha256_hex(&bad_manifest);
        let cache_dir = tempfile::tempdir().unwrap();
        let config = hosted_config(digest, cache_dir.path().to_path_buf());
        assets.insert("manifest.json".into(), bad_manifest);

        let fetcher = FakeFetcher::new(asset_responses(
            &info_url(&config),
            &release_dir(&config),
            &assets,
        ));
        let source = HostedReleaseSource::with_fetcher(config, fetcher);
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);
        let err = source.resolve(&cache).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("manifest must require baseline role"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn hosted_unsafe_archive_contents_are_rejected() {
        let mut encoder = flate2::write::GzEncoder::new(Vec::new(), flate2::Compression::default());
        {
            let mut builder = tar::Builder::new(&mut encoder);
            let mut header = tar::Header::new_gnu();
            header.set_entry_type(tar::EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_mtime(0);
            header.set_uid(0);
            header.set_gid(0);
            header.set_cksum();
            builder
                .append_data(&mut header, "evil-link", &[][..])
                .unwrap();
            builder.finish().unwrap();
        }
        let unsafe_archive = encoder.finish().unwrap();

        // Build a valid release but set the forge archive digest to the unsafe
        // archive's digest so the digest check passes and the unsafe-content
        // validation is the one that rejects it.
        let (_artifacts, _manifest_json, mut assets, _dir) = build_test_release();
        let mut manifest_value: serde_json::Value =
            serde_json::from_slice(&_manifest_json).unwrap();
        manifest_value["roles"]["forge"]["sha256"] =
            serde_json::Value::String(sha256_hex(&unsafe_archive));
        let manifest_json = serde_json::to_vec(&manifest_value).unwrap();
        let digest = sha256_hex(&manifest_json);
        let cache_dir = tempfile::tempdir().unwrap();
        let config = hosted_config(digest, cache_dir.path().to_path_buf());
        assets.insert(Role::Forge.archive_name(), unsafe_archive);
        assets.insert("manifest.json".into(), manifest_json);

        let fetcher = FakeFetcher::new(asset_responses(
            &info_url(&config),
            &release_dir(&config),
            &assets,
        ));
        let source = HostedReleaseSource::with_fetcher(config, fetcher);
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);
        let err = source.resolve(&cache).await.unwrap_err();
        assert!(
            err.to_string().contains("archive is unsafe") || err.to_string().contains("link"),
            "unexpected error: {err}"
        );
    }

    #[tokio::test]
    async fn hosted_oversized_response_is_rejected() {
        let (_artifacts, manifest_json, assets, _dir) = build_test_release();
        let digest = sha256_hex(&manifest_json);
        let cache_dir = tempfile::tempdir().unwrap();
        let mut config = hosted_config(digest, cache_dir.path().to_path_buf());
        config.max_download_bytes = 16; // tiny bound; even release metadata is oversized

        let fetcher = FakeFetcher::new(asset_responses(
            &info_url(&config),
            &release_dir(&config),
            &assets,
        ));
        let source = HostedReleaseSource::with_fetcher(config, fetcher);
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);
        let err = source.resolve(&cache).await.unwrap_err();
        assert!(
            err.to_string().contains("exceeds") || err.to_string().contains("request to"),
            "oversized/network error expected, got: {err}"
        );
    }

    #[tokio::test]
    async fn hosted_timeout_or_remote_failure_produces_clear_error() {
        let (_artifacts, manifest_json, assets, _dir) = build_test_release();
        let digest = sha256_hex(&manifest_json);
        let cache_dir = tempfile::tempdir().unwrap();
        let config = hosted_config(digest, cache_dir.path().to_path_buf());

        let mut responses = asset_responses(&info_url(&config), &release_dir(&config), &assets);
        let info = info_url(&config);
        responses.insert(info, Err(ReleaseFetchError("request timed out".into())));

        let fetcher = FakeFetcher::new(responses);
        let source = HostedReleaseSource::with_fetcher(config, fetcher);
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);
        let err = source.resolve(&cache).await.unwrap_err();
        assert!(
            err.to_string()
                .contains("cannot reach hosted release source"),
            "unexpected error: {err}"
        );
    }

    // -----------------------------------------------------------------------
    // Idempotency and caching.
    // -----------------------------------------------------------------------

    #[tokio::test]
    async fn repeated_resolution_of_same_reference_is_idempotent_and_uses_cache() {
        let (_artifacts, manifest_json, assets, _dir) = build_test_release();
        let digest = sha256_hex(&manifest_json);
        let cache_dir = tempfile::tempdir().unwrap();
        let config = hosted_config(digest.clone(), cache_dir.path().to_path_buf());
        let fetcher = FakeFetcher::new(asset_responses(
            &info_url(&config),
            &release_dir(&config),
            &assets,
        ));

        let source = HostedReleaseSource::with_fetcher(config.clone(), fetcher.clone());
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);

        let first = source.resolve(&cache).await.unwrap();
        let calls_after_first = fetcher.call_count();
        assert!(calls_after_first > 0, "first resolution must fetch");

        let second = source.resolve(&cache).await.unwrap();
        assert_eq!(
            fetcher.call_count(),
            calls_after_first,
            "a cache hit must not re-download"
        );

        assert_eq!(first.manifest_digest, second.manifest_digest);
        assert_eq!(first.source_reference, second.source_reference);
        assert_eq!(
            first.artifacts.archives.get(&Role::Nexus),
            second.artifacts.archives.get(&Role::Nexus)
        );

        let source2 = HostedReleaseSource::with_fetcher(config, fetcher.clone());
        let third = source2.resolve(&cache).await.unwrap();
        assert_eq!(third.manifest_digest, digest);
    }

    // -----------------------------------------------------------------------
    // Local source (no network).
    // -----------------------------------------------------------------------

    #[test]
    fn local_source_resolves_without_network() {
        let dir = tempfile::tempdir().unwrap();
        for role in Role::ALL {
            let role_dir = dir.path().join(format!("openflows-{}", role.as_str()));
            std::fs::create_dir_all(&role_dir).unwrap();
            std::fs::write(
                role_dir.join("main.tf"),
                format!("resource \"t\" {{ name = \"{}\" }}\n", role.as_str()),
            )
            .unwrap();
        }
        let source =
            LocalReleaseSource::new(dir.path().to_path_buf(), None, None, "2026.10.0".into());
        let resolved = source.resolve().unwrap();
        assert_eq!(resolved.artifacts.manifest.release, "2026.10.0");
        assert_eq!(resolved.manifest_digest.len(), 64);
        assert!(resolved.source_reference.starts_with("local://"));
        for role in Role::ALL {
            assert!(resolved.artifacts.archives.contains_key(&role));
        }
    }

    #[test]
    fn release_source_from_config_selects_and_resolves_local_without_network() {
        assert!(matches!(
            ReleaseSource::from_config(&crate::config::ReleaseSourceConfig::Local, None),
            Ok(ReleaseSource::Local(_))
        ));

        let dir = tempfile::tempdir().unwrap();
        for role in Role::ALL {
            let role_dir = dir.path().join(format!("openflows-{}", role.as_str()));
            std::fs::create_dir_all(&role_dir).unwrap();
            std::fs::write(
                role_dir.join("main.tf"),
                format!("resource \"t\" {{ name = \"{}\" }}\n", role.as_str()),
            )
            .unwrap();
        }
        let source =
            LocalReleaseSource::new(dir.path().to_path_buf(), None, None, "2026.10.0".into());
        let resolved = source.resolve().unwrap();
        assert_eq!(resolved.artifacts.manifest.release, "2026.10.0");
        assert!(resolved.source_reference.starts_with("local://"));
    }

    #[tokio::test]
    async fn cache_round_trip_stores_and_loads_verified_release() {
        let (artifacts, manifest_json, _assets, _dir) = build_test_release();
        let digest = sha256_hex(&manifest_json);
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = ReleaseCache::new(cache_dir.path().to_path_buf(), 8, 1024 * 1024);

        cache
            .store(&digest, &artifacts, "github://x/y@tag")
            .unwrap();

        let cached = cache.load(&digest).unwrap().expect("cached entry");
        assert_eq!(sha256_hex(&cached.manifest_json), digest);
        let manifest = TemplateManifest::parse(&cached.manifest_json).unwrap();
        let rebuilt = build_artifacts_from_cache(&manifest, cached.archives).unwrap();
        assert_eq!(
            rebuilt.archives.get(&Role::Nexus),
            artifacts.archives.get(&Role::Nexus)
        );
    }

    #[test]
    fn cache_repairs_corrupt_archives_and_preserves_valid_entries() {
        let (artifacts, manifest_json, _, _dir) = build_test_release();
        let digest = sha256_hex(&manifest_json);
        let dir = tempfile::tempdir().unwrap();
        let cache = ReleaseCache::new(dir.path().into(), 8, 1024 * 1024);
        cache.store(&digest, &artifacts, "test").unwrap();
        std::fs::write(
            cache.entry_dir(&digest).join(Role::Nexus.archive_name()),
            b"corrupt",
        )
        .unwrap();
        assert!(cache.load(&digest).unwrap().is_none());
        cache.store(&digest, &artifacts, "test").unwrap();
        cache.store(&digest, &artifacts, "test").unwrap();
        assert_eq!(
            cache.load(&digest).unwrap().unwrap().archives,
            artifacts.archives
        );
    }

    #[test]
    fn concurrent_cache_writers_publish_and_evict_successfully() {
        let (artifacts, _, _, _dir) = build_test_release();
        let dir = tempfile::tempdir().unwrap();
        let barrier = std::sync::Barrier::new(8);
        std::thread::scope(|scope| {
            for index in 0..8 {
                let mut artifacts = artifacts.clone();
                artifacts.manifest.release = format!("2026.10.{}", index / 2);
                let digest = sha256_hex(&artifacts.manifest.to_canonical_json().unwrap());
                let barrier = &barrier;
                let path = dir.path();
                scope.spawn(move || {
                    let cache = ReleaseCache::new(path.into(), 1, 1024 * 1024);
                    barrier.wait();
                    cache.store(&digest, &artifacts, "test").unwrap();
                });
            }
        });
        let cache = ReleaseCache::new(dir.path().into(), 1, 1024 * 1024);
        let entries: Vec<_> = std::fs::read_dir(cache.entries_dir()).unwrap().collect();
        assert_eq!(entries.len(), 1);
        let digest = entries[0]
            .as_ref()
            .unwrap()
            .file_name()
            .to_string_lossy()
            .into_owned();
        assert!(cache.load(&digest).unwrap().is_some());
    }
}
