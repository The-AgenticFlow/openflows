// crates/pocketflow-core/src/store.rs
//
// SharedStore — dual-backend (in-memory for dev, Redis for production).
// Same interface regardless of backend. Swap via REDIS_URL env var.

use anyhow::Result;
use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;
use std::{collections::HashMap, sync::Arc};
use tokio::sync::RwLock;
use tracing::{debug, trace};

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
            .filter(|k| {
                if pattern == "*" || pattern.ends_with('*') {
                    let prefix = pattern.trim_end_matches('*');
                    k.starts_with(prefix)
                } else {
                    k == &pattern
                }
            })
            .cloned()
            .collect()
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

    async fn keys(&self, pattern: &str) -> Vec<String> {
        use fred::types::scan::Scanner;
        use futures::StreamExt;
        let mut keys = Vec::new();
        let mut stream = self.client.scan(pattern, None, None);
        while let Some(result) = stream.next().await {
            if let Ok(mut scan_result) = result {
                if let Some(page) = scan_result.take_results() {
                    for key in page {
                        if let Some(s) = key.into_string() {
                            keys.push(s);
                        }
                    }
                }
                if scan_result.has_more() {
                    scan_result.next();
                }
            }
        }
        keys
    }

    async fn ping(&self) -> Result<()> {
        use fred::prelude::*;
        let _: String = self.client.ping(None).await?;
        Ok(())
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
                let raw: Option<String> = b.client.get(key).await.ok()?;
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
                    let _: core::result::Result<(), _> =
                        b.client.set::<(), _, _>(key, s, None, None, false).await;
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
                let _: core::result::Result<i64, _> = b.client.del(key).await;
            }
        }
    }

    async fn keys(&self, pattern: &str) -> Vec<String> {
        match self {
            Backend::InMemory(b) => b.keys(pattern).await,
            Backend::Redis(b) => b.keys(pattern).await,
        }
    }

    async fn ping(&self) -> Result<()> {
        match self {
            Backend::InMemory(_) => Ok(()),
            Backend::Redis(b) => b.ping().await,
        }
    }
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
                let claimed: i64 = b.client.eval(
                    "if redis.call('SET', KEYS[1], ARGV[1], 'NX', 'EX', ARGV[2]) then return 1 else return 0 end",
                    vec![key],
                    vec![token.to_string(), ttl_secs.to_string()],
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
                let _: i64 = b.client.eval(
                    "if redis.call('GET', KEYS[1]) == ARGV[1] then return redis.call('DEL', KEYS[1]) else return 0 end",
                    vec![key],
                    vec![token.to_string()],
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
    pub async fn raw_keys(&self, pattern: &str) -> Vec<String> {
        self.backend.keys(pattern).await
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
                let raw: Option<String> = b.client.get(&key).await?;
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
                let raw: Option<String> = b.client.get(&key).await?;
                let current =
                    Lifecycle::decode(raw.as_deref().map(serde_json::from_str).transpose()?)?;
                anyhow::ensure!(
                    current.version == version,
                    "Lifecycle changed; refresh before retrying"
                );
                let next = current.apply(actor, event, ts)?;
                let script="local old=redis.call('GET',KEYS[1]); if (old or '') ~= ARGV[1] then return 0 end; redis.call('SET',KEYS[1],ARGV[2]); return 1";
                let changed: i64 = b
                    .client
                    .eval(
                        script,
                        vec![key],
                        vec![raw.unwrap_or_default(), serde_json::to_string(&next)?],
                    )
                    .await?;
                anyhow::ensure!(changed == 1, "Lifecycle changed; refresh before retrying");
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
