//! Tenant Redis ACL and namespace lifecycle.

use crate::error::ManagerError;
use async_trait::async_trait;
use fred::prelude::*;
use fred::types::{ClusterHash, CustomCommand};

#[async_trait]
pub trait RedisIsolationBackend: Send + Sync {
    async fn ensure_tenant(
        &self,
        username: &str,
        password: &str,
        namespace: &str,
    ) -> Result<(), ManagerError>;
    async fn revoke_tenant(&self, username: &str) -> Result<(), ManagerError>;
    async fn clear_namespace(&self, namespace: &str) -> Result<u64, ManagerError>;
}

/// Deterministic in-memory policy backend for unit/integration tests. It
/// records ACL boundaries without pretending to be a public isolation layer.
#[derive(Default)]
pub struct InMemoryRedisIsolation {
    tenants: std::sync::Mutex<std::collections::HashMap<String, String>>,
}

#[async_trait]
impl RedisIsolationBackend for InMemoryRedisIsolation {
    async fn ensure_tenant(
        &self,
        username: &str,
        _password: &str,
        namespace: &str,
    ) -> Result<(), ManagerError> {
        self.tenants
            .lock()
            .unwrap()
            .insert(username.to_string(), namespace.to_string());
        Ok(())
    }

    async fn revoke_tenant(&self, username: &str) -> Result<(), ManagerError> {
        self.tenants.lock().unwrap().remove(username);
        Ok(())
    }

    async fn clear_namespace(&self, _namespace: &str) -> Result<u64, ManagerError> {
        Ok(0)
    }
}

#[derive(Clone)]
pub struct FredRedisIsolation {
    client: fred::clients::Client,
}

impl FredRedisIsolation {
    pub async fn connect(url: &str) -> Result<Self, ManagerError> {
        let config = fred::types::config::Config::from_url(url)
            .map_err(|e| ManagerError::Config(format!("invalid Redis admin URL: {e}")))?;
        let client = fred::types::Builder::from_config(config)
            .build()
            .map_err(|e| ManagerError::Config(format!("cannot build Redis admin client: {e}")))?;
        client
            .init()
            .await
            .map_err(|e| ManagerError::Config(format!("cannot connect Redis admin client: {e}")))?;
        Ok(FredRedisIsolation { client })
    }

    async fn acl(&self, args: Vec<String>) -> Result<(), ManagerError> {
        let command = CustomCommand::new("ACL", ClusterHash::Random, false);
        self.client
            .custom::<fred::types::Value, _>(command, args)
            .await
            .map_err(|e| {
                ManagerError::Service(anyhow::anyhow!("Redis ACL operation failed: {e}"))
            })?;
        Ok(())
    }
}

#[async_trait]
impl RedisIsolationBackend for FredRedisIsolation {
    async fn ensure_tenant(
        &self,
        username: &str,
        password: &str,
        namespace: &str,
    ) -> Result<(), ManagerError> {
        self.acl(vec![
            "SETUSER".into(),
            username.into(),
            "reset".into(),
            "on".into(),
            format!(">{password}"),
            format!("~ns:{namespace}:*"),
            format!("&openflows:{namespace}:*"),
            "+@read".into(),
            "+@write".into(),
            "+ping".into(),
            "+publish".into(),
            "+subscribe".into(),
            "+eval".into(),
            "+evalsha".into(),
            "-keys".into(),
            "-scan".into(),
            "-acl".into(),
            "-config".into(),
            "-shutdown".into(),
        ])
        .await
    }

    async fn revoke_tenant(&self, username: &str) -> Result<(), ManagerError> {
        self.acl(vec!["DELUSER".into(), username.into()]).await
    }

    async fn clear_namespace(&self, namespace: &str) -> Result<u64, ManagerError> {
        use fred::types::scan::Scanner;
        use futures_util::StreamExt;
        let pattern = format!("ns:{namespace}:*");
        let mut stream = self.client.scan(pattern, Some(500), None);
        let mut keys = Vec::new();
        while let Some(page) = stream.next().await {
            let mut page = page.map_err(|e| {
                ManagerError::Service(anyhow::anyhow!("Redis namespace scan failed: {e}"))
            })?;
            if let Some(values) = page.take_results() {
                keys.extend(values.into_iter().filter_map(|value| value.into_string()));
            }
            if page.has_more() {
                page.next();
            }
        }
        if keys.is_empty() {
            return Ok(0);
        }
        self.client.del::<u64, _>(keys).await.map_err(|e| {
            ManagerError::Service(anyhow::anyhow!("Redis namespace cleanup failed: {e}"))
        })
    }
}
