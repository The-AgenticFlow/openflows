//! Platform-only release approval, promotion, and retirement.

use crate::coder::CoderAdapter;
use crate::error::ManagerError;
use crate::id::{OrganizationId, ReleaseId, UserId};
use crate::templates::repository::{ReleaseRepository, ReleaseRow};
use crate::templates::service::{ReleaseArtifacts, ReleaseService};
use sqlx::PgPool;

#[derive(Clone)]
pub struct ReleaseApprovalService {
    pub pool: PgPool,
    pub releases: ReleaseService,
    pub repo: ReleaseRepository,
}

impl ReleaseApprovalService {
    pub fn new(pool: PgPool, releases: ReleaseService, repo: ReleaseRepository) -> Self {
        ReleaseApprovalService {
            pool,
            releases,
            repo,
        }
    }

    /// Whether a user is a designated platform operator. Ordinary customer
    /// organization admins are not.
    pub async fn is_platform_operator(&self, user_id: UserId) -> Result<bool, ManagerError> {
        let row: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT user_id FROM platform_operators WHERE user_id = $1")
                .bind(user_id.0)
                .fetch_optional(&self.pool)
                .await
                .map_err(ManagerError::from)?;
        Ok(row.is_some())
    }

    /// Grant platform-operator status. Only an existing platform operator may
    /// do this (the `created_by` actor must already be an operator).
    pub async fn grant_platform_operator(
        &self,
        actor: UserId,
        target: UserId,
    ) -> Result<(), ManagerError> {
        if !self.is_platform_operator(actor).await? {
            return Err(ManagerError::api(
                "PLATFORM_OPERATOR_REQUIRED",
                "only a platform operator may grant platform operator status",
            ));
        }
        sqlx::query(
            "INSERT INTO platform_operators (user_id, created_by)
             VALUES ($1, $2) ON CONFLICT (user_id) DO NOTHING",
        )
        .bind(target.0)
        .bind(actor.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// List releases (sanitized: never include credentials).
    pub async fn list_releases(&self) -> Result<Vec<ReleaseRow>, ManagerError> {
        let rows = sqlx::query_as::<_, ReleaseRow>(
            "SELECT id, version, manifest_sha256, artifact_uri, status,
                    approved_by, approved_at, created_at
               FROM template_releases
              ORDER BY created_at DESC",
        )
        .fetch_all(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(rows)
    }

    /// Approve a staged release (platform operator only). A release must be
    /// approved before it can be promoted to an organization.
    pub async fn approve(
        &self,
        operator: UserId,
        release_id: ReleaseId,
    ) -> Result<(), ManagerError> {
        self.require_platform_operator(operator).await?;
        self.releases.approve_release(release_id, operator.0).await
    }

    /// Promote an approved release to an organization (platform operator only).
    pub async fn promote(
        &self,
        operator: UserId,
        release_id: ReleaseId,
        organization_id: OrganizationId,
        adapter: &dyn CoderAdapter,
        artifacts: &ReleaseArtifacts,
    ) -> Result<(), ManagerError> {
        self.require_platform_operator(operator).await?;
        self.releases
            .promote_to_org(organization_id, release_id, adapter, artifacts)
            .await
    }

    /// Retire an approved release so it is no longer promoted to new
    /// organizations (platform operator only). Existing tenants keep their
    /// pinned releases.
    pub async fn retire(
        &self,
        operator: UserId,
        release_id: ReleaseId,
    ) -> Result<(), ManagerError> {
        self.require_platform_operator(operator).await?;
        let (row, _) = self
            .repo
            .get_release(release_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("template release"))?;
        if row.status != "approved" {
            return Err(ManagerError::Conflict(
                "only an approved release can be retired".into(),
            ));
        }
        self.repo
            .set_release_status(release_id, "retired", None)
            .await?;
        Ok(())
    }

    async fn require_platform_operator(&self, operator: UserId) -> Result<(), ManagerError> {
        if !self.is_platform_operator(operator).await? {
            return Err(ManagerError::api(
                "PLATFORM_OPERATOR_REQUIRED",
                "customer users cannot approve or promote platform releases",
            ));
        }
        Ok(())
    }
}
