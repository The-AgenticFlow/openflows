//! Durable organization-provisioning worker.
//!
//! Claims leased `org.provision` operations and runs the organization readiness
//! reconciliation. It keeps external Coder calls outside database
//! transactions, persists progress step-by-step, fences stale lease holders,
//! and never marks a suspended/deleted organization ready. A failed import,
//! missing license or missing provisioner heartbeat produces an
//! explicit not-ready state with a sanitized error code.

use crate::coder::CoderAdapter;
use crate::error::ManagerError;
use crate::id::{OperationId, OrganizationId};
use crate::operations::{self, LeaseGuard, OperationState};
use crate::repositories::OrganizationsRepository;
use crate::templates::packaging::Role;
use crate::templates::repository::ReleaseRepository;
use crate::templates::service::{internal_org_name, ReleaseService};
use sqlx::PgPool;
use std::sync::Arc;
use std::time::Duration;

/// How long the worker sleeps between claim cycles.
const POLL_INTERVAL: Duration = Duration::from_secs(2);

/// Readiness step keys (persisted as `operations.current_step`).
pub mod step {
    pub const VERIFY_CAPABILITIES: &str = "verify_capabilities";
    pub const ENSURE_ORG: &str = "ensure_org";
    pub const ENSURE_PROVISIONER: &str = "ensure_provisioner";
    pub const PUBLISH_TEMPLATES: &str = "publish_templates";
    pub const MARK_READY: &str = "mark_ready";
}

/// Error codes surfaced as sanitized not-ready reasons.
pub mod err {
    pub const RELEASE_NOT_CONFIGURED: &str = "RELEASE_NOT_CONFIGURED";
    pub const CODER_CAPABILITIES_UNMET: &str = "CODER_CAPABILITIES_UNMET";
    pub const CODER_ORG_FAILED: &str = "CODER_ORG_FAILED";
    pub const PROVISIONER_NOT_READY: &str = "PROVISIONER_NOT_READY";
    pub const IMPORT_FAILED: &str = "IMPORT_FAILED";
    pub const ORG_STATE_CHANGED: &str = "ORG_STATE_CHANGED";
}

/// The outcome of one provisioning attempt, returned for tests to assert on
/// without running the polling loop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OrgProvisionOutcome {
    Ready(OrganizationId),
    NotReady(OrganizationId, &'static str),
    Failed(OrganizationId, &'static str, bool),
    StaleLease,
}

#[derive(Clone)]
pub struct OrgProvisionWorker {
    pub pool: PgPool,
    pub orgs: OrganizationsRepository,
    pub releases: ReleaseService,
    pub release_repo: ReleaseRepository,
    pub adapter: Arc<dyn CoderAdapter>,
    /// The operator-configured approved release artifacts. `None` means no
    /// release is configured; readiness must not proceed.
    pub artifacts: Option<Arc<crate::templates::service::ReleaseArtifacts>>,
    /// The release id of the operator-configured approved release.
    pub release_id: Option<crate::id::ReleaseId>,
    /// The provisioner backend adapter name (operator config).
    pub provisioner_backend: String,
    /// The Coder version baseline required for readiness.
    pub version_baseline: String,
}

impl OrgProvisionWorker {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        pool: PgPool,
        orgs: OrganizationsRepository,
        releases: ReleaseService,
        release_repo: ReleaseRepository,
        adapter: Arc<dyn CoderAdapter>,
        artifacts: Option<Arc<crate::templates::service::ReleaseArtifacts>>,
        release_id: Option<crate::id::ReleaseId>,
        provisioner_backend: String,
        version_baseline: String,
    ) -> Self {
        OrgProvisionWorker {
            pool,
            orgs,
            releases,
            release_repo,
            adapter,
            artifacts,
            release_id,
            provisioner_backend,
            version_baseline,
        }
    }

    pub async fn run_forever(&self) -> Result<(), ManagerError> {
        loop {
            if let Err(e) = self.run_once().await {
                tracing::warn!(%e, "org provision worker poll failed; retrying");
            }
            tokio::time::sleep(POLL_INTERVAL).await;
        }
    }

    /// Claim and process one `org.provision` operation once.
    pub async fn run_once(&self) -> Result<(), ManagerError> {
        let owner = format!("org-provision-worker-{}", std::process::id());
        let Some(guard) = operations::claim_kind(&self.pool, &owner, Some("org.provision")).await?
        else {
            return Ok(());
        };
        let heartbeat_guard = guard.clone();
        let mut heartbeat = tokio::task::JoinSet::new();
        heartbeat.spawn(async move {
            let mut interval = tokio::time::interval(
                operations::HEARTBEAT_INTERVAL
                    .to_std()
                    .unwrap_or(Duration::from_secs(10)),
            );
            loop {
                interval.tick().await;
                if !heartbeat_guard.heartbeat().await? {
                    return Err(ManagerError::Service(anyhow::anyhow!(
                        "org provision worker lost its lease"
                    )));
                }
            }
            #[allow(unreachable_code)]
            Ok::<(), ManagerError>(())
        });
        let result = self.provision(&guard).await;
        heartbeat.abort_all();
        self.finish(&guard, result).await?;
        Ok(())
    }

    /// Execute one provisioning attempt for a leased operation.
    pub async fn provision(&self, guard: &LeaseGuard) -> Result<OrgProvisionOutcome, ManagerError> {
        let org_id = guard.organization_id;

        // 1. Re-read the organization. If it is no longer provisioning (e.g.
        //    suspended/deleted during a previous attempt), do not resurrect it.
        let org_status = self.org_status(org_id).await?;
        let Some(org_status) = org_status else {
            return Ok(OrgProvisionOutcome::NotReady(
                org_id,
                err::ORG_STATE_CHANGED,
            ));
        };
        if org_status != "provisioning" {
            return Ok(OrgProvisionOutcome::NotReady(
                org_id,
                err::ORG_STATE_CHANGED,
            ));
        }

        // 2. Release prerequisites.
        let artifacts = match &self.artifacts {
            Some(a) => a.clone(),
            None => {
                self.record_not_ready(
                    guard,
                    org_id,
                    err::RELEASE_NOT_CONFIGURED,
                    "no approved release configured",
                )
                .await?;
                return Ok(OrgProvisionOutcome::NotReady(
                    org_id,
                    err::RELEASE_NOT_CONFIGURED,
                ));
            }
        };
        let release_id = match self.release_id {
            Some(r) => r,
            None => {
                self.record_not_ready(guard, org_id, err::RELEASE_NOT_CONFIGURED, "no release id")
                    .await?;
                return Ok(OrgProvisionOutcome::NotReady(
                    org_id,
                    err::RELEASE_NOT_CONFIGURED,
                ));
            }
        };
        let release_approved = self
            .releases
            .repo
            .get_release(release_id)
            .await?
            .map(|(row, _)| row.status == "approved")
            .unwrap_or(false);
        if !release_approved {
            self.record_not_ready(
                guard,
                org_id,
                err::RELEASE_NOT_CONFIGURED,
                "release is not approved",
            )
            .await?;
            return Ok(OrgProvisionOutcome::NotReady(
                org_id,
                err::RELEASE_NOT_CONFIGURED,
            ));
        }

        // 3. Capabilities.
        if !guard.set_step(step::VERIFY_CAPABILITIES).await? {
            return Ok(OrgProvisionOutcome::StaleLease);
        }
        let caps = self.adapter.verify_capabilities().await?;
        if !caps.version_ok || !caps.license_ok {
            let code = if !caps.version_ok {
                "CODER_VERSION_UNMET"
            } else {
                err::CODER_CAPABILITIES_UNMET
            };
            self.record_not_ready(
                guard,
                org_id,
                code,
                "Coder version or license prerequisite unmet",
            )
            .await?;
            return Ok(OrgProvisionOutcome::NotReady(org_id, code));
        }

        // 4. Ensure the dedicated Coder organization and persist its mapping.
        if !guard.set_step(step::ENSURE_ORG).await? {
            return Ok(OrgProvisionOutcome::StaleLease);
        }
        let internal_name = internal_org_name(org_id);
        let (mapped, display_name): (Option<String>, String) = sqlx::query_as(
            "SELECT coder_organization_id, display_name FROM organizations WHERE id = $1",
        )
        .bind(org_id.0)
        .fetch_one(&self.pool)
        .await?;
        let coder_org = if let Some(id) = mapped {
            coder_client::CoderOrganizationId::new(id)
        } else {
            match self
                .adapter
                .ensure_organization(&internal_name, &display_name)
                .await
            {
                Ok(org) => org,
                Err(e) => {
                    self.record_not_ready(guard, org_id, err::CODER_ORG_FAILED, &e.to_string())
                        .await?;
                    return Ok(OrgProvisionOutcome::NotReady(org_id, err::CODER_ORG_FAILED));
                }
            }
        };
        self.orgs
            .set_coder_organization_id(crate::db::tx::OrgScope::new(org_id), org_id, &coder_org.0)
            .await?;

        // 5. Ensure the provisioner (heartbeat verified).
        if !guard.set_step(step::ENSURE_PROVISIONER).await? {
            return Ok(OrgProvisionOutcome::StaleLease);
        }
        self.release_repo
            .upsert_provisioner(
                crate::id::ProvisionerId::from_uuid(org_id.0),
                org_id,
                &format!("openflows-provisioner-{}", coder_org.0),
                None,
                "provisioning",
            )
            .await?;
        if let Err(e) = self
            .adapter
            .ensure_provisioner_ready(&coder_org, &self.provisioner_backend)
            .await
        {
            self.set_provisioner_status_fenced(guard, org_id, "unavailable")
                .await?;
            self.record_not_ready(guard, org_id, err::PROVISIONER_NOT_READY, &e.to_string())
                .await?;
            return Ok(OrgProvisionOutcome::NotReady(
                org_id,
                err::PROVISIONER_NOT_READY,
            ));
        }
        self.set_provisioner_status_fenced(guard, org_id, "ready")
            .await?;

        // 6. Publish the required template roles. A failed or timed-out import
        //    prevents readiness.
        if !guard.set_step(step::PUBLISH_TEMPLATES).await? {
            return Ok(OrgProvisionOutcome::StaleLease);
        }
        if let Err(e) = self
            .releases
            .publish_for_org(org_id, release_id, self.adapter.as_ref(), &artifacts)
            .await
        {
            let terminal = matches!(&e, ManagerError::Api(api) if api.code == err::IMPORT_FAILED || api.code == "IMPORT_CANCELLED");
            let code = err::IMPORT_FAILED;
            self.record_not_ready(guard, org_id, code, &e.to_string())
                .await?;
            if terminal {
                return Ok(OrgProvisionOutcome::Failed(org_id, code, false));
            }
            return Ok(OrgProvisionOutcome::NotReady(org_id, code));
        }

        // Organization provisioning ends with successful template publication.
        // Model configuration is independent of organization readiness.
        if !guard.set_step(step::MARK_READY).await? {
            return Ok(OrgProvisionOutcome::StaleLease);
        }
        // Readiness, release selection and operation completion commit together.
        // Lock the organization first so suspension/deletion cannot race this write.
        let mut tx = self.pool.begin().await?;
        let status: String =
            sqlx::query_scalar("SELECT status FROM organizations WHERE id = $1 FOR UPDATE")
                .bind(org_id.0)
                .fetch_one(&mut *tx)
                .await?;
        if status != "provisioning" {
            return Ok(OrgProvisionOutcome::NotReady(
                org_id,
                err::ORG_STATE_CHANGED,
            ));
        }
        if !guard.lock_in_tx(&mut tx).await? {
            return Ok(OrgProvisionOutcome::StaleLease);
        }
        let approved: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM template_releases WHERE id = $1 AND status = 'approved' FOR SHARE",
        )
        .bind(release_id.0)
        .fetch_optional(&mut *tx)
        .await?;
        if approved.is_none() {
            return Ok(OrgProvisionOutcome::NotReady(
                org_id,
                err::RELEASE_NOT_CONFIGURED,
            ));
        }
        sqlx::query("UPDATE organizations SET status = 'ready',
            provisioner_status = 'ready', last_readiness_error = NULL, updated_at = now() WHERE id = $1")
            .bind(org_id.0).execute(&mut *tx).await?;
        ReleaseRepository::bind_org_release_in_tx(&mut tx, org_id, release_id).await?;
        if !guard
            .complete_in_tx(
                &mut tx,
                serde_json::json!({"status":"ready", "organization_id":org_id.to_string()}),
            )
            .await?
        {
            return Ok(OrgProvisionOutcome::StaleLease);
        }
        tx.commit().await?;
        tracing::info!(org_id = %org_id.0, "organization marked ready");
        Ok(OrgProvisionOutcome::Ready(org_id))
    }

    async fn finish(
        &self,
        guard: &LeaseGuard,
        outcome: Result<OrgProvisionOutcome, ManagerError>,
    ) -> Result<(), ManagerError> {
        match outcome {
            Ok(OrgProvisionOutcome::Ready(_)) => {} // Completed atomically in provision().
            Ok(OrgProvisionOutcome::NotReady(org_id, code)) => {
                // Not-ready is a retryable failure: prerequisites may later be
                // met (e.g. provisioner becomes available).
                let ok = guard
                    .fail(code != err::ORG_STATE_CHANGED, Some(code))
                    .await?;
                if ok {
                    self.orgs
                        .set_last_readiness_error(
                            crate::db::tx::OrgScope::new(org_id),
                            org_id,
                            Some(code),
                        )
                        .await?;
                }
            }
            Ok(OrgProvisionOutcome::Failed(org_id, code, retryable)) => {
                let ok = guard.fail(retryable, Some(code)).await?;
                if ok && !retryable {
                    self.orgs
                        .set_last_readiness_error(
                            crate::db::tx::OrgScope::new(org_id),
                            org_id,
                            Some(code),
                        )
                        .await?;
                }
            }
            Ok(OrgProvisionOutcome::StaleLease) => {}
            Err(e) => {
                tracing::warn!(%e, "org provision attempt failed with error");
                let _ = guard.fail(true, None).await?;
            }
        }
        Ok(())
    }

    async fn org_status(&self, org_id: OrganizationId) -> Result<Option<String>, ManagerError> {
        let row: Option<(String,)> =
            sqlx::query_as("SELECT status FROM organizations WHERE id = $1")
                .bind(org_id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(ManagerError::from)?;
        Ok(row.map(|r| r.0))
    }

    async fn record_not_ready(
        &self,
        guard: &LeaseGuard,
        org_id: OrganizationId,
        code: &str,
        message: &str,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await?;
        if guard.lock_in_tx(&mut tx).await? {
            sqlx::query("UPDATE organizations SET last_readiness_error = $1, updated_at = now() WHERE id = $2 AND status = 'provisioning'")
                .bind(code).bind(org_id.0).execute(&mut *tx).await?;
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        tracing::warn!(org_id = %org_id.0, code, message, "organization not ready");
        Ok(())
    }

    async fn set_provisioner_status_fenced(
        &self,
        guard: &LeaseGuard,
        org_id: OrganizationId,
        status: &str,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await?;
        sqlx::query("SELECT id FROM organizations WHERE id = $1 FOR UPDATE")
            .bind(org_id.0)
            .fetch_one(&mut *tx)
            .await?;
        if guard.lock_in_tx(&mut tx).await? {
            sqlx::query(
                "UPDATE organization_provisioners SET status = $1,
                    last_heartbeat_at = CASE WHEN $1 = 'ready' THEN now() ELSE last_heartbeat_at END,
                    updated_at = now() WHERE organization_id = $2",
            )
            .bind(status)
            .bind(org_id.0)
            .execute(&mut *tx)
            .await?;
            tx.commit().await?;
        } else {
            tx.rollback().await?;
        }
        Ok(())
    }
}

/// A placeholder used by tests to assert operation state transitions.
pub async fn set_operation_state(
    pool: &PgPool,
    id: OperationId,
    state: OperationState,
) -> Result<bool, ManagerError> {
    let affected =
        sqlx::query("UPDATE operations SET state = $1, updated_at = now() WHERE id = $2")
            .bind(state.as_str())
            .bind(id.0)
            .execute(pool)
            .await
            .map_err(ManagerError::from)?
            .rows_affected();
    Ok(affected == 1)
}

/// The role order used for publication (Nexus..Lore).
pub const PUBLISH_ROLES: [Role; 4] = [Role::Nexus, Role::Forge, Role::Sentinel, Role::Vessel];
