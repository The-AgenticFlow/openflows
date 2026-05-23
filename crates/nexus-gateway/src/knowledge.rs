use anyhow::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};

/// Knowledge entry for RAG-based command interpretation.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KnowledgeEntry {
    pub id: String,
    pub content: String,
    pub category: String,
    pub metadata: serde_json::Value,
}

/// Trait for a knowledge store that can be used to augment command interpretation.
#[async_trait]
pub trait KnowledgeStore: Send + Sync {
    /// Search for relevant command patterns / documentation
    async fn search(&self, query: &str, limit: usize) -> Result<Vec<KnowledgeEntry>>;

    /// Ingest a new knowledge entry
    async fn ingest(&self, entry: KnowledgeEntry) -> Result<()>;
}

/// Stub implementation — returns empty results, no-op ingest.
pub struct StubKnowledgeStore;

#[async_trait]
impl KnowledgeStore for StubKnowledgeStore {
    async fn search(&self, _query: &str, _limit: usize) -> Result<Vec<KnowledgeEntry>> {
        Ok(vec![])
    }

    async fn ingest(&self, _entry: KnowledgeEntry) -> Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_stub_knowledge_store() {
        let store = StubKnowledgeStore;
        let results = store.search("pause workflow", 5).await.unwrap();
        assert!(results.is_empty());

        let entry = KnowledgeEntry {
            id: "1".to_string(),
            content: "test".to_string(),
            category: "test".to_string(),
            metadata: serde_json::Value::Null,
        };
        store.ingest(entry).await.unwrap();
    }
}
