//! Tenant Redis ACL and namespace lifecycle.

use crate::error::ManagerError;
use async_trait::async_trait;
use config::{Ticket, TicketStatus, WorkerSlot};
use fred::prelude::*;
use fred::types::{ClusterHash, CustomCommand};
use std::collections::{HashMap, HashSet};

#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct NamespaceCleanResult {
    pub tickets_reset: u64,
    pub worker_slots_cleared: u64,
    pub recovery_counters_cleared: u64,
}

#[async_trait]
pub trait RedisIsolationBackend: Send + Sync {
    async fn ensure_tenant(
        &self,
        username: &str,
        password: &str,
        namespace: &str,
    ) -> Result<(), ManagerError>;
    async fn revoke_tenant(&self, username: &str) -> Result<(), ManagerError>;
    async fn clean_namespace(
        &self,
        namespace: &str,
        reset_all: bool,
    ) -> Result<NamespaceCleanResult, ManagerError>;
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

    async fn clean_namespace(
        &self,
        _namespace: &str,
        _reset_all: bool,
    ) -> Result<NamespaceCleanResult, ManagerError> {
        Ok(NamespaceCleanResult::default())
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

fn tenant_acl_args(username: &str, password: &str, namespace: &str) -> Vec<String> {
    vec![
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
        // Key patterns cannot constrain commands with no key arguments.
        // Revoke database-wide writes after granting @write so a tenant
        // credential cannot erase another tenant's namespaced state.
        "-flushdb".into(),
        "-flushall".into(),
        "-acl".into(),
        "-config".into(),
        "-shutdown".into(),
    ]
}

fn reset_tenant_state(
    tickets: &mut [Ticket],
    slots: &mut HashMap<String, WorkerSlot>,
    reset_all: bool,
) -> (HashSet<String>, usize) {
    let mut reset = HashSet::new();
    for ticket in tickets {
        if reset_all
            || matches!(
                &ticket.status,
                TicketStatus::AwaitingHuman { .. } | TicketStatus::Failed { .. }
            )
        {
            ticket.status = TicketStatus::Open;
            ticket.attempts = 0;
            reset.insert(ticket.id.clone());
        }
    }

    let before = slots.len();
    if reset_all {
        slots.clear();
    } else {
        slots.retain(|_, slot| {
            slot.status
                .ticket_id()
                .is_none_or(|ticket_id| !reset.contains(ticket_id))
        });
    }
    (reset, before - slots.len())
}

#[async_trait]
impl RedisIsolationBackend for FredRedisIsolation {
    async fn ensure_tenant(
        &self,
        username: &str,
        password: &str,
        namespace: &str,
    ) -> Result<(), ManagerError> {
        self.acl(tenant_acl_args(username, password, namespace))
            .await
    }

    async fn revoke_tenant(&self, username: &str) -> Result<(), ManagerError> {
        self.acl(vec!["DELUSER".into(), username.into()]).await
    }

    async fn clean_namespace(
        &self,
        namespace: &str,
        reset_all: bool,
    ) -> Result<NamespaceCleanResult, ManagerError> {
        let tickets_key = format!("ns:{namespace}:tickets");
        let slots_key = format!("ns:{namespace}:worker_slots");
        let index_key = format!("ns:{namespace}:_keys");
        let tickets_raw: Option<String> = self.client.get(&tickets_key).await.map_err(|e| {
            ManagerError::Service(anyhow::anyhow!("Redis tenant tickets read failed: {e}"))
        })?;
        let slots_raw: Option<String> = self.client.get(&slots_key).await.map_err(|e| {
            ManagerError::Service(anyhow::anyhow!("Redis worker slots read failed: {e}"))
        })?;
        let mut tickets: Vec<Ticket> = tickets_raw
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| {
                ManagerError::Service(anyhow::anyhow!("Redis tenant tickets are invalid: {e}"))
            })?
            .unwrap_or_default();
        let mut slots: HashMap<String, WorkerSlot> = slots_raw
            .as_deref()
            .map(serde_json::from_str)
            .transpose()
            .map_err(|e| {
                ManagerError::Service(anyhow::anyhow!("Redis worker slots are invalid: {e}"))
            })?
            .unwrap_or_default();
        let (reset, slots_cleared) = reset_tenant_state(&mut tickets, &mut slots, reset_all);

        if !reset.is_empty() {
            let encoded =
                serde_json::to_string(&tickets).map_err(|e| ManagerError::Service(e.into()))?;
            self.client
                .set::<(), _, _>(&tickets_key, encoded, None, None, false)
                .await
                .map_err(|e| {
                    ManagerError::Service(anyhow::anyhow!(
                        "Redis tenant tickets update failed: {e}"
                    ))
                })?;
            self.client
                .sadd::<i64, _, _>(&index_key, &tickets_key)
                .await
                .map_err(|e| {
                    ManagerError::Service(anyhow::anyhow!("Redis tenant index update failed: {e}"))
                })?;
        }
        if slots_cleared > 0 {
            let encoded =
                serde_json::to_string(&slots).map_err(|e| ManagerError::Service(e.into()))?;
            self.client
                .set::<(), _, _>(&slots_key, encoded, None, None, false)
                .await
                .map_err(|e| {
                    ManagerError::Service(anyhow::anyhow!("Redis worker slots update failed: {e}"))
                })?;
            self.client
                .sadd::<i64, _, _>(&index_key, &slots_key)
                .await
                .map_err(|e| {
                    ManagerError::Service(anyhow::anyhow!("Redis tenant index update failed: {e}"))
                })?;
        }

        let recovery_keys = if reset_all {
            use fred::types::scan::Scanner;
            use futures_util::StreamExt;
            let pattern = format!("ns:{namespace}:ticket:*:recovery_attempts");
            let mut stream = self.client.scan(pattern, Some(500), None);
            let mut keys = Vec::new();
            while let Some(page) = stream.next().await {
                let mut page = page.map_err(|e| {
                    ManagerError::Service(anyhow::anyhow!(
                        "Redis recovery counter scan failed: {e}"
                    ))
                })?;
                if let Some(values) = page.take_results() {
                    keys.extend(values.into_iter().filter_map(|value| value.into_string()));
                }
                if page.has_more() {
                    page.next();
                }
            }
            keys
        } else {
            reset
                .iter()
                .map(|ticket_id| format!("ns:{namespace}:ticket:{ticket_id}:recovery_attempts"))
                .collect()
        };
        let mut recovery_counters_cleared = 0;
        for key in recovery_keys {
            recovery_counters_cleared += self.client.del::<u64, _>(&key).await.map_err(|e| {
                ManagerError::Service(anyhow::anyhow!(
                    "Redis recovery counter cleanup failed: {e}"
                ))
            })?;
            self.client
                .srem::<i64, _, _>(&index_key, &key)
                .await
                .map_err(|e| {
                    ManagerError::Service(anyhow::anyhow!("Redis tenant index cleanup failed: {e}"))
                })?;
        }

        Ok(NamespaceCleanResult {
            tickets_reset: reset.len() as u64,
            worker_slots_cleared: slots_cleared as u64,
            recovery_counters_cleared,
        })
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

#[cfg(test)]
mod tests {
    use super::*;
    use config::WorkerStatus;

    #[test]
    fn tenant_acl_revokes_keyless_global_writes() {
        let args = tenant_acl_args("tenant", "secret", "namespace");
        let write = args.iter().position(|arg| arg == "+@write").unwrap();
        for denied in ["-flushdb", "-flushall"] {
            let position = args.iter().position(|arg| arg == denied).unwrap();
            assert!(
                position > write,
                "{denied} must override the earlier write-category grant"
            );
        }
    }

    #[test]
    fn normal_clean_resets_only_stale_tickets_and_their_slots() {
        let mut tickets = vec![
            Ticket {
                id: "stale".into(),
                title: "stale".into(),
                body: String::new(),
                priority: 1,
                branch: None,
                status: TicketStatus::AwaitingHuman {
                    worker_id: "forge-1".into(),
                    reason: "blocked".into(),
                    attempts: 3,
                },
                issue_url: None,
                attempts: 3,
            },
            Ticket {
                id: "active".into(),
                title: "active".into(),
                body: String::new(),
                priority: 2,
                branch: None,
                status: TicketStatus::InProgress {
                    worker_id: "forge-2".into(),
                },
                issue_url: None,
                attempts: 1,
            },
        ];
        let mut slots = HashMap::from([
            (
                "forge-1".into(),
                WorkerSlot {
                    id: "forge-1".into(),
                    status: WorkerStatus::Working {
                        ticket_id: "stale".into(),
                        issue_url: None,
                    },
                    workspace_id: Some("ws-stale".into()),
                },
            ),
            (
                "forge-2".into(),
                WorkerSlot {
                    id: "forge-2".into(),
                    status: WorkerStatus::Working {
                        ticket_id: "active".into(),
                        issue_url: None,
                    },
                    workspace_id: Some("ws-active".into()),
                },
            ),
        ]);

        let (reset, cleared) = reset_tenant_state(&mut tickets, &mut slots, false);

        assert_eq!(reset, HashSet::from(["stale".to_string()]));
        assert_eq!(cleared, 1);
        assert!(matches!(tickets[0].status, TicketStatus::Open));
        assert!(matches!(tickets[1].status, TicketStatus::InProgress { .. }));
        assert_eq!(slots.len(), 1);
        assert!(slots.contains_key("forge-2"));
    }

    #[test]
    fn clean_all_resets_every_ticket_and_slot() {
        let mut tickets = vec![Ticket {
            id: "active".into(),
            title: "active".into(),
            body: String::new(),
            priority: 1,
            branch: None,
            status: TicketStatus::InProgress {
                worker_id: "forge-1".into(),
            },
            issue_url: None,
            attempts: 2,
        }];
        let mut slots = HashMap::from([(
            "forge-1".into(),
            WorkerSlot {
                id: "forge-1".into(),
                status: WorkerStatus::Working {
                    ticket_id: "active".into(),
                    issue_url: None,
                },
                workspace_id: Some("ws-active".into()),
            },
        )]);

        let (reset, cleared) = reset_tenant_state(&mut tickets, &mut slots, true);

        assert_eq!(reset.len(), 1);
        assert_eq!(cleared, 1);
        assert!(matches!(tickets[0].status, TicketStatus::Open));
        assert_eq!(tickets[0].attempts, 0);
        assert!(slots.is_empty());
    }
}
