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
}

// ── SharedStore (public API) ──────────────────────────────────────────────

#[derive(Clone)]
pub struct SharedStore {
    backend: Backend,
    ring_buffer: Arc<RwLock<Vec<StoreEvent>>>,
}

impl SharedStore {
    /// In-memory backend — use for dev and tests.
    pub fn new_in_memory() -> Self {
        Self {
            backend: Backend::InMemory(Arc::new(InMemoryBackend::new())),
            ring_buffer: Arc::new(RwLock::new(Vec::with_capacity(RING_BUFFER_SIZE))),
        }
    }

    /// Redis backend — use for Docker Compose and production.
    pub async fn new_redis(url: &str) -> Result<Self> {
        Ok(Self {
            backend: Backend::Redis(Arc::new(RedisBackend::new(url).await?)),
            ring_buffer: Arc::new(RwLock::new(Vec::with_capacity(RING_BUFFER_SIZE))),
        })
    }

    // ── Core get/set/del ─────────────────────────────────────────────

    pub async fn get(&self, key: &str) -> Option<Value> {
        let v = self.backend.get(key).await;
        // Per-key gets fire at very high cadence (polling watchers hit
        // every artifact every ~200ms per pair).  Demote successful-and-miss
        // probe logs to trace so they stop burying diagnostic output.  Adding
        // keys via `set` / `del` are infrequent and stay at debug.
        trace!(key, found = v.is_some(), "store.get");
        v
    }

    pub async fn set(&self, key: &str, value: Value) {
        debug!(key, "store.set");
        self.backend.set(key, value).await;
    }

    pub async fn del(&self, key: &str) {
        debug!(key, "store.del");
        self.backend.del(key).await;
    }

    pub async fn keys(&self, pattern: &str) -> Vec<String> {
        self.backend.keys(pattern).await
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
