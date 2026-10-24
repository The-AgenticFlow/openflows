// crates/pocketflow-core/src/store.rs
//
// SharedStore — dual-backend (in-memory for dev, Redis for production).
// Same interface regardless of backend. Swap via REDIS_URL env var.

use anyhow::{Context, Result};
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::{collections::HashMap, future::Future, sync::Arc, time::Duration};
use tokio::sync::RwLock;
use tracing::{debug, trace, warn};

// ── Event ring buffer ─────────────────────────────────────────────────────

const RING_BUFFER_SIZE: usize = 1000;

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct StoreEvent {
    pub agent: String,
    pub event_type: String,
    pub payload: Value,
    pub ts: u64, // unix millis
}

// ── In-memory backend ─────────────────────────────────────────────────────

struct InMemoryBackend {
    map: RwLock<HashMap<String, Value>>,
}

impl InMemoryBackend {
    fn new() -> Self {
        Self {
            map: RwLock::new(HashMap::new()),
        }
    }

    async fn keys(&self, pattern: &str) -> Vec<String> {
        let map = self.map.read().await;
        map.keys()
            .filter(|k| glob_match(pattern, k))
            .cloned()
            .collect()
    }

    async fn scan_keys(&self, pattern: &str) -> Vec<String> {
        self.keys(pattern).await
    }
}

// ── Redis backend ─────────────────────────────────────────────────────────
struct RedisBackend {
    client: fred::clients::Client,
}

impl RedisBackend {
    async fn new(url: &str) -> Result<Self> {
        use fred::prelude::*;
        let config = Config::from_url(url)?;
        let client = Builder::from_config(config).build()?;
        client.init().await?;
        Ok(Self { client })
    }

    /// Tenant-scoped key lookup. The per-tenant SET index is populated on
    /// every writer (see `Backend::set` and `Backend::transition`), so this
    /// avoids SCAN entirely — required for the ACL-restricted tenant users
    /// (`-keys -scan`). Patterns are matched as globs against the stored keys.
    ///
    /// When the index yields nothing, a best-effort SCAN backfills keys that
    /// predate the index (e.g. written before this code was deployed). On a
    /// tenant-ACL connection that SCAN is denied and simply returns empty.
    async fn keys(&self, pattern: &str) -> Vec<String> {
        use fred::interfaces::SetsInterface;
        let Some(index) = tenant_index_key(pattern) else {
            return Vec::new();
        };
        let keys: Vec<String> = self
            .client
            .smembers(index.clone())
            .await
            .unwrap_or_default();
        let mut matches: Vec<String> = keys
            .into_iter()
            .filter(|key| glob_match(pattern, key))
            .collect();
        if matches.is_empty() {
            for key in self.scan_keys(pattern).await {
                if glob_match(pattern, &key) && !matches.contains(&key) {
                    matches.push(key.clone());
                    let _ = self.client.sadd::<i64, _, _>(index.clone(), key).await;
                }
            }
        }
        matches
    }

    /// Cross-tenant / whole-store enumeration using SCAN. This is only used by
    /// `raw_keys`, which callers invoke on an admin (non-tenant-ACL) connection,
    /// so SCAN is permitted. It handles middle wildcards and existing keys that
    /// predate the per-tenant index.
    async fn scan_keys(&self, pattern: &str) -> Vec<String> {
        use fred::types::scan::Scanner;
        use futures::StreamExt;
        let mut out = Vec::new();
        let mut stream = self.client.scan(pattern, None, None);
        while let Some(result) = stream.next().await {
            if let Ok(mut scan_result) = result {
                if let Some(page) = scan_result.take_results() {
                    for key in page {
                        if let Some(s) = key.into_string() {
                            out.push(s);
                        }
                    }
                }
                if scan_result.has_more() {
                    scan_result.next();
                }
            }
        }
        out
    }

    async fn ping(&self) -> Result<()> {
        use fred::prelude::*;
        let _: String = self.client.ping(None).await?;
        Ok(())
    }
}

/// Minimal glob matcher compatible with the patterns used by Redis key
/// enumeration: `*` matches any run of characters, `?` matches a single
/// character, and everything else matches literally.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let t: Vec<char> = text.chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let (mut star, mut mark) = (usize::MAX, 0usize);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '?' || p[pi] == t[ti]) {
            pi += 1;
            ti += 1;
        } else if pi < p.len() && p[pi] == '*' {
            star = pi;
            mark = ti;
            pi += 1;
        } else if star != usize::MAX {
            pi = star + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    while pi < p.len() && p[pi] == '*' {
        pi += 1;
    }
    pi == p.len()
}

fn redis_op_timeout() -> Duration {
    let secs = std::env::var("OPENFLOWS_REDIS_OP_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|value| *value > 0)
        .unwrap_or(5);
    Duration::from_secs(secs)
}

async fn bounded_redis<T, F, E>(operation: &'static str, future: F) -> Result<T>
where
    F: Future<Output = core::result::Result<T, E>>,
    E: Into<anyhow::Error>,
{
    match tokio::time::timeout(redis_op_timeout(), future).await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(error.into()).with_context(|| format!("Redis {operation} failed")),
        Err(_) => anyhow::bail!(
            "Redis {operation} timed out after {}s",
            redis_op_timeout().as_secs()
        ),
    }
}

// ── Backend enum ──────────────────────────────────────────────────────────

#[derive(Clone)]
enum Backend {
    InMemory(Arc<InMemoryBackend>),
    Redis(Arc<RedisBackend>),
}

impl Backend {
    async fn get(&self, key: &str) -> Option<Value> {
        match self {
            Backend::InMemory(b) => b.map.read().await.get(key).cloned(),
            Backend::Redis(b) => {
                use fred::prelude::*;
                let raw: Option<String> = match bounded_redis("GET", b.client.get(key)).await {
                    Ok(raw) => raw,
                    Err(error) => {
                        warn!(key, %error, "Redis get failed");
                        return None;
                    }
                };
                raw.and_then(|s| serde_json::from_str(&s).ok())
            }
        }
    }

    async fn set(&self, key: &str, value: Value) {
        match self {
            Backend::InMemory(b) => {
                b.map.write().await.insert(key.to_string(), value);
            }
            Backend::Redis(b) => {
                use fred::prelude::*;
                if let Ok(s) = serde_json::to_string(&value) {
                    if let Err(error) =
                        bounded_redis("SET", b.client.set::<(), _, _>(key, s, None, None, false))
                            .await
                    {
                        warn!(key, %error, "Redis set failed");
                    }
                    if let Some(index) = tenant_index_key(key) {
                        let _ = b.client.sadd::<i64, _, _>(index, key).await;
                    }
                }
            }
        }
    }

    async fn del(&self, key: &str) {
        match self {
            Backend::InMemory(b) => {
                b.map.write().await.remove(key);
            }
            Backend::Redis(b) => {
                use fred::prelude::*;
                if let Err(error) = bounded_redis::<i64, _, _>("DEL", b.client.del(key)).await {
                    warn!(key, %error, "Redis del failed");
                }
                if let Some(index) = tenant_index_key(key) {
                    let _ = b.client.srem::<i64, _, _>(index, key).await;
                }
            }
        }
    }

    async fn keys(&self, pattern: &str) -> Vec<String> {
        match self {
            Backend::InMemory(b) => b.keys(pattern).await,
            Backend::Redis(b) => {
                match tokio::time::timeout(redis_op_timeout(), b.keys(pattern)).await {
                    Ok(keys) => keys,
                    Err(_) => {
                        warn!(
                            pattern,
                            timeout_secs = redis_op_timeout().as_secs(),
                            "Redis keys timed out"
                        );
                        Vec::new()
                    }
                }
            }
        }
    }

    /// SCAN-based enumeration used by `raw_keys`. Only call on an admin
    /// connection where SCAN is permitted by the Redis ACL.
    async fn scan_keys(&self, pattern: &str) -> Vec<String> {
        match self {
            Backend::InMemory(b) => b.scan_keys(pattern).await,
            Backend::Redis(b) => {
                match tokio::time::timeout(redis_op_timeout(), b.scan_keys(pattern)).await {
                    Ok(keys) => keys,
                    Err(_) => {
                        warn!(
                            pattern,
                            timeout_secs = redis_op_timeout().as_secs(),
                            "Redis scan_keys timed out"
                        );
                        Vec::new()
                    }
                }
            }
        }
    }

    async fn ping(&self) -> Result<()> {
        match self {
            Backend::InMemory(_) => Ok(()),
            Backend::Redis(b) => tokio::time::timeout(redis_op_timeout(), b.ping())
                .await
                .context("Redis PING timed out")?,
        }
    }
}

fn tenant_index_key(key: &str) -> Option<String> {
    let rest = key.strip_prefix("ns:")?;
    let tenant = rest.split_once(':')?.0;
    Some(format!("ns:{tenant}:_keys"))
}

// ── SharedStore (public API) ──────────────────────────────────────────────

#[derive(Clone)]
pub struct SharedStore {
    backend: Backend,
    ring_buffer: Arc<RwLock<Vec<StoreEvent>>>,
    tenant: String,
}

impl SharedStore {
    /// In-memory backend — use for dev and tests.
    pub fn new_in_memory() -> Self {
        Self::new_in_memory_with_tenant("default")
    }

    /// In-memory backend with explicit tenant — for testing multi-tenancy.
    pub fn new_in_memory_with_tenant(tenant: impl Into<String>) -> Self {
        Self {
            backend: Backend::InMemory(Arc::new(InMemoryBackend::new())),
            ring_buffer: Arc::new(RwLock::new(Vec::with_capacity(RING_BUFFER_SIZE))),
            tenant: tenant.into(),
        }
    }

    /// Redis backend — use for Docker Compose and production.
    /// Tenant is derived from the OPENFLOWS_TENANT env var, or "default" if unset.
    /// This ensures all keys are namespaced as `ns:{tenant}:*` for tenant isolation.
    pub async fn new_redis(url: &str) -> Result<Self> {
        Self::new_redis_with_tenant(url, None).await
    }

    /// Redis backend with explicit or derived tenant.
    /// If tenant is None, reads from OPENFLOWS_TENANT env var.
    pub async fn new_redis_with_tenant(url: &str, tenant: Option<String>) -> Result<Self> {
        let resolved_tenant = if let Some(t) = tenant {
            t
        } else {
            config::EnvConfig::from_env()
                .map(|e| e.tenant.effective_tenant().to_string())
                .unwrap_or_else(|_| "default".to_string())
        };

        Ok(Self {
            backend: Backend::Redis(Arc::new(RedisBackend::new(url).await?)),
            ring_buffer: Arc::new(RwLock::new(Vec::with_capacity(RING_BUFFER_SIZE))),
            tenant: resolved_tenant,
        })
    }

    /// Build a tenant-namespaced key: `ns:{tenant}:{key}`.
    fn ns_key(&self, key: &str) -> String {
        format!("ns:{}:{}", self.tenant, key)
    }

    // ── Core get/set/del ─────────────────────────────────────────────

    pub async fn get(&self, key: &str) -> Option<Value> {
        let ns_key = self.ns_key(key);
        let v = self.backend.get(&ns_key).await;
        trace!(key = %ns_key, found = v.is_some(), "store.get");
        v
    }

    pub async fn set(&self, key: &str, value: Value) {
        let ns_key = self.ns_key(key);
        debug!(key = %ns_key, "store.set");
        self.backend.set(&ns_key, value).await;
    }

    pub async fn del(&self, key: &str) {
        let ns_key = self.ns_key(key);
        debug!(key = %ns_key, "store.del");
        self.backend.del(&ns_key).await;
    }

    /// Atomically acquire a tenant-scoped lease. Use a unique token per attempt.
    /// Returns false while another lease is live; backend errors are propagated.
    pub async fn try_claim(&self, key: &str, token: &str, ttl_secs: u64) -> Result<bool> {
        anyhow::ensure!(ttl_secs > 0, "Lease TTL must be positive");
        let key = self.ns_key(key);
        match &self.backend {
            Backend::InMemory(b) => {
                let mut map = b.map.write().await;
                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)?
                    .as_millis();
                let expires_at = now + u128::from(ttl_secs) * 1000;
                let expires_at = u64::try_from(expires_at)?;
                if let Some(existing) = map.get(&key) {
                    // Unknown values also occupy the key; never overwrite them.
                    if existing
                        .get("expires_at")
                        .and_then(Value::as_u64)
                        .is_none_or(|expiry| u128::from(expiry) > now)
                    {
                        return Ok(false);
                    }
                }
                map.insert(
                    key,
                    serde_json::json!({"token": token, "expires_at": expires_at}),
                );
                Ok(true)
            }
            Backend::Redis(b) => {
                use fred::prelude::*;
                let claimed: i64 = bounded_redis(
                    "lease acquire",
                    b.client.eval(
                        "if redis.call('SET', KEYS[1], ARGV[1], 'NX', 'EX', ARGV[2]) then return 1 else return 0 end",
                        vec![key],
                        vec![token.to_string(), ttl_secs.to_string()],
                    ),
                ).await?;
                Ok(claimed == 1)
            }
        }
    }

    /// Release a lease only if its current token matches. Missing leases are harmless.
    pub async fn release_claim(&self, key: &str, token: &str) -> Result<()> {
        let key = self.ns_key(key);
        match &self.backend {
            Backend::InMemory(b) => {
                let mut map = b.map.write().await;
                if map
                    .get(&key)
                    .and_then(|value| value.get("token"))
                    .and_then(Value::as_str)
                    == Some(token)
                {
                    map.remove(&key);
                }
            }
            Backend::Redis(b) => {
                use fred::prelude::*;
                let _: i64 = bounded_redis(
                    "lease release",
                    b.client.eval(
                        "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) else return 0 end",
                        vec![key],
                        vec![token.to_string()],
                    ),
                ).await?;
            }
        }
        Ok(())
    }

    pub async fn keys(&self, pattern: &str) -> Vec<String> {
        // For pattern matching, we need to handle both the namespace prefix
        // and the fact that SCAN returns full keys. The pattern should match
        // against the namespaced form: ns:{tenant}:{pattern}
        let ns_pattern = self.ns_key(pattern);
        self.backend.keys(&ns_pattern).await
    }

    /// Raw key scan on the backend WITHOUT tenant namespacing.
    /// `pattern` is matched as-is against full Redis keys (e.g. "ns:*").
    /// Returns the full Redis keys that matched.
    ///
    /// Unlike `keys`, this uses SCAN and therefore requires a connection with
    /// SCAN permission (the admin URL). It supports cross-tenant and middle
    /// wildcard patterns and finds keys that predate the per-tenant index.
    pub async fn raw_keys(&self, pattern: &str) -> Vec<String> {
        self.backend.scan_keys(pattern).await
    }

    /// Raw delete of a full Redis key WITHOUT tenant namespacing.
    /// Use this with keys returned by `keys()` / `raw_keys()`, which are
    /// already fully-qualified and must not be re-prefixed.
    pub async fn raw_del(&self, key: &str) {
        self.backend.del(key).await;
    }

    /// Check whether the underlying store backend is reachable.
    pub async fn ping(&self) -> Result<()> {
        self.backend.ping().await
    }

    /// Typed get — deserialises JSON into T. Returns None on missing key or type mismatch.
    pub async fn get_typed<T: DeserializeOwned>(&self, key: &str) -> Option<T> {
        let v = self.get(key).await?;
        serde_json::from_value(v).ok()
    }

    /// Typed set — serialises T to JSON Value.
    pub async fn set_typed<T: Serialize>(&self, key: &str, value: &T) -> Result<()> {
        let v = serde_json::to_value(value)?;
        self.set(key, v).await;
        Ok(())
    }

    /// Strict lifecycle reads distinguish an absent ticket from a store outage.
    pub async fn lifecycle(&self, ticket: &str) -> Result<config::lifecycle::Lifecycle> {
        let key = self.ns_key(&format!("ticket:{ticket}:status"));
        let value = match &self.backend {
            Backend::InMemory(b) => b.map.read().await.get(&key).cloned(),
            Backend::Redis(b) => {
                use fred::prelude::*;
                let raw: Option<String> =
                    bounded_redis("lifecycle GET", b.client.get(&key)).await?;
                raw.map(|s| serde_json::from_str(&s)).transpose()?
            }
        };
        config::lifecycle::Lifecycle::decode(value)
    }

    /// Compare-and-set the entire lifecycle, including approvals and evidence.
    /// A conflict is returned to the caller; stale requests are never replayed.
    pub async fn transition(
        &self,
        ticket: &str,
        version: u64,
        actor: &str,
        event: config::lifecycle::Event,
    ) -> Result<config::lifecycle::Lifecycle> {
        use config::lifecycle::Lifecycle;
        let key = self.ns_key(&format!("ticket:{ticket}:status"));
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)?
            .as_secs();
        match &self.backend {
            Backend::InMemory(b) => {
                let mut map = b.map.write().await;
                let current = Lifecycle::decode(map.get(&key).cloned())?;
                anyhow::ensure!(
                    current.version == version,
                    "Lifecycle changed; refresh before retrying"
                );
                let next = current.apply(actor, event, ts)?;
                map.insert(key, serde_json::to_value(&next)?);
                Ok(next)
            }
            Backend::Redis(b) => {
                use fred::prelude::*;
                let raw: Option<String> =
                    bounded_redis("transition GET", b.client.get(&key)).await?;
                let current =
                    Lifecycle::decode(raw.as_deref().map(serde_json::from_str).transpose()?)?;
                anyhow::ensure!(
                    current.version == version,
                    "Lifecycle changed; refresh before retrying"
                );
                let next = current.apply(actor, event, ts)?;
                let script="local old=redis.call('GET',KEYS[1]); if (old or '') ~= ARGV[1] then return 0 end; redis.call('SET',KEYS[1],ARGV[2]); return 1";
                let changed: i64 = bounded_redis(
                    "transition CAS",
                    b.client.eval(
                        script,
                        vec![key.clone()],
                        vec![raw.unwrap_or_default(), serde_json::to_string(&next)?],
                    ),
                )
                .await?;
                anyhow::ensure!(changed == 1, "Lifecycle changed; refresh before retrying");
                // Keep the per-tenant key index in sync so `keys()` scans can
                // find lifecycle status keys written here rather than via `set`.
                if let Some(index) = tenant_index_key(&key) {
                    use fred::interfaces::SetsInterface;
                    let _ = b.client.sadd::<i64, _, _>(index, &key).await;
                }
                Ok(next)
            }
        }
    }

    // ── Event ring buffer ─────────────────────────────────────────────

    /// Emit a structured event. Every node lifecycle phase should call this.
    pub async fn emit(&self, agent: &str, event_type: &str, payload: Value) {
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as u64;

        let event = StoreEvent {
            agent: agent.to_string(),
            event_type: event_type.to_string(),
            payload,
            ts,
        };

        let mut buf = self.ring_buffer.write().await;
        if buf.len() >= RING_BUFFER_SIZE {
            buf.remove(0); // drop oldest
        }
        buf.push(event);
    }

    /// Returns all events since `cursor` (index). Used by the TUI tail loop.
    pub async fn get_events_since(&self, cursor: usize) -> Vec<StoreEvent> {
        let buf = self.ring_buffer.read().await;
        if cursor >= buf.len() {
            return vec![];
        }
        buf[cursor..].to_vec()
    }

    /// Number of events in the ring buffer (for initial TUI render).
    pub async fn event_count(&self) -> usize {
        self.ring_buffer.read().await.len()
    }
}

#[cfg(test)]
mod lifecycle_tests {
    use super::*;
    use config::lifecycle::{Event, Phase};
    #[tokio::test]
    async fn competing_updates_cannot_both_commit() {
        let store = SharedStore::new_in_memory();
        let event = Event::Plan {
            content: "plan".into(),
        };
        let (a, b) = tokio::join!(
            store.transition("T-1", 0, "forge", event.clone()),
            store.transition("T-1", 0, "forge", event)
        );
        assert_ne!(a.is_ok(), b.is_ok());
        assert_eq!(store.lifecycle("T-1").await.unwrap().version, 1);
    }
    #[tokio::test]
    async fn legacy_approval_is_not_authority_and_merged_stays_terminal() {
        let store = SharedStore::new_in_memory();
        store
            .set("ticket:T-1:status", serde_json::json!("approved"))
            .await;
        assert_eq!(store.lifecycle("T-1").await.unwrap().phase, Phase::Planning);
        store
            .set("ticket:T-1:status", serde_json::json!("Merged"))
            .await;
        assert!(store
            .transition(
                "T-1",
                0,
                "forge",
                Event::Move {
                    phase: Phase::Planning,
                    head: None
                }
            )
            .await
            .is_err());
    }
}

#[cfg(test)]
mod claim_tests {
    use super::*;

    #[tokio::test]
    async fn concurrent_claims_have_one_winner() {
        let store = SharedStore::new_in_memory();
        let mut tasks = tokio::task::JoinSet::new();
        for i in 0..32 {
            let store = store.clone();
            tasks.spawn(async move {
                store
                    .try_claim("review:T-1", &i.to_string(), 120)
                    .await
                    .unwrap()
            });
        }
        let mut winners = 0;
        while let Some(result) = tasks.join_next().await {
            winners += usize::from(result.unwrap());
        }
        assert_eq!(winners, 1);
    }

    #[tokio::test]
    async fn release_requires_current_owner() {
        let store = SharedStore::new_in_memory();
        assert!(store.try_claim("review:T-1", "owner", 120).await.unwrap());
        store.release_claim("review:T-1", "other").await.unwrap();
        assert!(!store.try_claim("review:T-1", "other", 120).await.unwrap());
        store.release_claim("review:T-1", "owner").await.unwrap();
        assert!(store.try_claim("review:T-1", "other", 120).await.unwrap());
        store.release_claim("review:T-1", "owner").await.unwrap();
        assert!(!store.try_claim("review:T-1", "third", 120).await.unwrap());
    }

    #[tokio::test]
    async fn expired_claim_can_be_replaced_and_old_owner_cannot_release_it() {
        let store = SharedStore::new_in_memory();
        store
            .set(
                "review:T-1",
                serde_json::json!({"token": "old", "expires_at": 0}),
            )
            .await;
        assert!(store.try_claim("review:T-1", "new", 120).await.unwrap());
        store.release_claim("review:T-1", "old").await.unwrap();
        assert!(!store.try_claim("review:T-1", "third", 120).await.unwrap());
    }

    #[tokio::test]
    async fn claims_are_namespaced_and_reject_zero_ttl() {
        let store = SharedStore::new_in_memory_with_tenant("one");
        let mut other = store.clone();
        other.tenant = "two".into();
        assert!(store.try_claim("review:T-1", "owner", 0).await.is_err());
        assert!(store.try_claim("review:T-1", "owner", 120).await.unwrap());
        assert!(other.try_claim("review:T-1", "owner", 120).await.unwrap());
    }
}

#[cfg(test)]
mod redis_index_tests {
    use super::*;
    use config::lifecycle::Event;

    /// These tests need a live Redis; point `OPENFLOWS_TEST_REDIS_URL` at one
    /// (e.g. `redis://127.0.0.1:6399`). They are skipped when it is unset.
    fn redis_url() -> Option<String> {
        std::env::var("OPENFLOWS_TEST_REDIS_URL").ok()
    }

    #[tokio::test]
    async fn index_covers_set_and_transition_and_raw_keys_scan() {
        let Some(url) = redis_url() else {
            eprintln!("skipping: OPENFLOWS_TEST_REDIS_URL unset");
            return;
        };
        let store = SharedStore::new_redis_with_tenant(&url, Some("ten-a".into()))
            .await
            .unwrap();
        store.ping().await.unwrap();

        // `set` populates the per-tenant index.
        store
            .set("ticket:T1:chat:forge", serde_json::json!("chat-1"))
            .await;
        assert!(
            store
                .keys("ticket:*")
                .await
                .iter()
                .any(|k| k.contains("T1:chat")),
            "keys() should find keys written via set"
        );

        // `transition` must also maintain the index so lifecycle status keys
        // written directly are discoverable (review comment 31).
        store
            .transition(
                "T1",
                0,
                "forge",
                Event::Plan {
                    content: "p".into(),
                },
            )
            .await
            .unwrap();
        assert!(
            store
                .keys("ticket:*")
                .await
                .iter()
                .any(|k| k.contains("T1:status")),
            "keys() should find the lifecycle status key written via transition"
        );

        // Cross-tenant enumeration must work through raw_keys (comment 32).
        let raw = store.raw_keys("ns:*").await;
        assert!(
            raw.iter().any(|k| k.starts_with("ns:ten-a:")),
            "raw_keys(ns:*) should see tenant keys, got: {raw:?}"
        );

        // Middle wildcards must match through raw_keys (comment 32).
        store
            .set("ticket:T1:recovery_attempts", serde_json::json!(0))
            .await;
        let recovery = store.raw_keys("ns:ten-a:ticket:*:recovery_attempts").await;
        assert!(
            recovery.iter().any(|k| k.contains("T1:recovery_attempts")),
            "middle wildcard raw_keys should find recovery counters, got: {recovery:?}"
        );
    }

    /// The tenant path must keep working for an ACL-restricted user that has
    /// `-keys -scan` denied, because `keys()` reads the per-tenant SET index
    /// (only falling back to SCAN when the index is empty). Point
    /// `OPENFLOWS_TEST_REDIS_URL` at a Redis where a user
    /// `tenant-test`/`testpass` exists with the tenant ACL from redis.rs.
    #[tokio::test]
    async fn keys_works_without_scan_acl() {
        let Some(url) = redis_url() else {
            eprintln!("skipping: OPENFLOWS_TEST_REDIS_URL unset");
            return;
        };
        let store = SharedStore::new_redis_with_tenant(&url, Some("ten-a".into()))
            .await
            .unwrap();
        store.ping().await.unwrap();
        // Seed via the admin-style URL first, then read back through the
        // restricted user below if the base URL already carries those creds.
        let restricted = if url.contains("tenant-test") {
            Some(store)
        } else {
            None
        };
        if let Some(store) = restricted {
            store
                .set("ticket:T2:chat:forge", serde_json::json!("chat-2"))
                .await;
            let found = store.keys("ticket:*").await;
            assert!(
                found.iter().any(|k| k.contains("T2:chat")),
                "keys() must resolve via the index for a -scan user, got: {found:?}"
            );
        } else {
            eprintln!("skipping: OPENFLOWS_TEST_REDIS_URL has no tenant-test credentials");
        }
    }
}
