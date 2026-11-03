//! Persistence for template releases, per-organization publication, and
//! organization provisioners.
//!
//! Reuses the existing schema (`template_releases`, `template_release_roles`,
//! `organization_template_versions`, `organization_provisioners`) plus the
//! organization release bindings. All reads are organization-scoped.

use crate::error::ManagerError;
use crate::id::{OrganizationId, ProvisionerId, ReleaseId};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::collections::BTreeMap;

use super::packaging::{Role, RoleManifestEntry, TemplateManifest};

/// A persisted template release (from `template_releases`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReleaseRow {
    pub id: uuid::Uuid,
    pub version: String,
    pub manifest_sha256: String,
    pub artifact_uri: String,
    pub status: String,
    pub approved_by: Option<uuid::Uuid>,
    pub approved_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

/// The persisted roles of a release (`template_release_roles`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReleaseRoleRow {
    pub release_id: uuid::Uuid,
    pub role: String,
    pub archive_digest: String,
    pub parameter_schema_version: i32,
    pub image_digest: Option<String>,
}

/// A per-organization template publication (`organization_template_versions`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OrgTemplateVersionRow {
    pub id: uuid::Uuid,
    pub organization_id: uuid::Uuid,
    pub release_id: uuid::Uuid,
    pub role: String,
    pub coder_template_id: Option<String>,
    pub coder_version_id: Option<String>,
    pub import_job_id: Option<String>,
    pub status: String,
    pub error_code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// An organization provisioner (`organization_provisioners`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OrgProvisionerRow {
    pub id: uuid::Uuid,
    pub organization_id: uuid::Uuid,
    pub backend_resource_id: Option<String>,
    pub coder_provisioner_ref: Option<String>,
    pub secret_ref: Option<String>,
    pub status: String,
    pub last_heartbeat_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct ReleaseRepository {
    pool: PgPool,
}

impl ReleaseRepository {
    pub fn new(pool: PgPool) -> Self {
        ReleaseRepository { pool }
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    /// Insert an immutable release and its role archives in one transaction.
    /// Duplicate version or digest is a conflict.
    pub async fn insert_release(
        &self,
        release_id: ReleaseId,
        manifest: &TemplateManifest,
        artifact_uri: &str,
        manifest_sha256: &str,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;
        sqlx::query(
            "INSERT INTO template_releases
                (id, version, manifest_sha256, artifact_uri, status)
             VALUES ($1, $2, $3, $4, 'staged')",
        )
        .bind(release_id.0)
        .bind(&manifest.release)
        .bind(manifest_sha256)
        .bind(artifact_uri)
        .execute(&mut *tx)
        .await
        .map_err(|e| translate_release_insert(e, &manifest.release))?;

        for (role, entry) in &manifest.roles {
            sqlx::query(
                "INSERT INTO template_release_roles
                    (release_id, role, archive_digest, parameter_schema_version, image_digest)
                 VALUES ($1, $2, $3, $4, $5)",
            )
            .bind(release_id.0)
            .bind(role.as_str())
            .bind(&entry.sha256)
            .bind(entry.parameter_schema_version as i32)
            .bind(&entry.image_digest)
            .execute(&mut *tx)
            .await
            .map_err(ManagerError::from)?;
        }
        tx.commit().await.map_err(ManagerError::from)?;
        Ok(())
    }

    /// Look up a release by its immutable manifest digest.
    pub async fn get_release_by_digest(
        &self,
        manifest_sha256: &str,
    ) -> Result<Option<(ReleaseRow, BTreeMap<Role, RoleManifestEntry>)>, ManagerError> {
        let row = sqlx::query_as::<_, ReleaseRow>(
            "SELECT id, version, manifest_sha256, artifact_uri, status,
                    approved_by, approved_at, created_at
               FROM template_releases
              WHERE manifest_sha256 = $1",
        )
        .bind(manifest_sha256)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        match row {
            Some(row) => {
                let roles = self.load_roles(row.id).await?;
                Ok(Some((row, roles)))
            }
            None => Ok(None),
        }
    }

    /// Look up a release by its operator-visible version.
    ///
    /// Versions are immutable deployment labels. This lookup lets callers
    /// distinguish a harmless restart of an already-imported release from an
    /// attempt to reuse a version for different template bytes.
    pub async fn get_release_by_version(
        &self,
        version: &str,
    ) -> Result<Option<(ReleaseRow, BTreeMap<Role, RoleManifestEntry>)>, ManagerError> {
        let row = sqlx::query_as::<_, ReleaseRow>(
            "SELECT id, version, manifest_sha256, artifact_uri, status,
                    approved_by, approved_at, created_at
               FROM template_releases
              WHERE version = $1",
        )
        .bind(version)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        match row {
            Some(row) => {
                let roles = self.load_roles(row.id).await?;
                Ok(Some((row, roles)))
            }
            None => Ok(None),
        }
    }

    /// Fetch a release by id.
    pub async fn get_release(
        &self,
        release_id: ReleaseId,
    ) -> Result<Option<(ReleaseRow, BTreeMap<Role, RoleManifestEntry>)>, ManagerError> {
        let row = sqlx::query_as::<_, ReleaseRow>(
            "SELECT id, version, manifest_sha256, artifact_uri, status,
                    approved_by, approved_at, created_at
               FROM template_releases
              WHERE id = $1",
        )
        .bind(release_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        match row {
            Some(row) => {
                let roles = self.load_roles(row.id).await?;
                Ok(Some((row, roles)))
            }
            None => Ok(None),
        }
    }

    async fn load_roles(
        &self,
        release_id: uuid::Uuid,
    ) -> Result<BTreeMap<Role, RoleManifestEntry>, ManagerError> {
        let rows = sqlx::query_as::<_, ReleaseRoleRow>(
            "SELECT release_id, role, archive_digest, parameter_schema_version, image_digest
               FROM template_release_roles
              WHERE release_id = $1",
        )
        .bind(release_id)
        .fetch_all(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        let mut map = BTreeMap::new();
        for r in rows {
            let role = match r.role.as_str() {
                "nexus" => Role::Nexus,
                "forge" => Role::Forge,
                "sentinel" => Role::Sentinel,
                "vessel" => Role::Vessel,
                "lore" => Role::Lore,
                other => {
                    return Err(ManagerError::Service(anyhow::anyhow!(
                        "unknown role in database: {other}"
                    )))
                }
            };
            map.insert(
                role,
                RoleManifestEntry {
                    role,
                    archive: role.archive_name(),
                    sha256: r.archive_digest,
                    parameter_schema_version: r.parameter_schema_version as u32,
                    image_digest: r.image_digest,
                },
            );
        }
        Ok(map)
    }

    /// Set a release's status (approval/promotion/retirement). Returns false if
    /// the release does not exist or the transition is invalid (guarded in the
    /// service layer).
    pub async fn set_release_status(
        &self,
        release_id: ReleaseId,
        status: &str,
        approved_by: Option<uuid::Uuid>,
    ) -> Result<bool, ManagerError> {
        let affected = if approved_by.is_some() {
            sqlx::query(
                "UPDATE template_releases
                    SET status = $1, approved_by = $2, approved_at = now(), updated_at = now()
                  WHERE id = $3",
            )
            .bind(status)
            .bind(approved_by)
            .bind(release_id.0)
            .execute(&self.pool)
            .await
            .map_err(ManagerError::from)?
            .rows_affected()
        } else {
            sqlx::query(
                "UPDATE template_releases
                    SET status = $1, updated_at = now()
                  WHERE id = $2",
            )
            .bind(status)
            .bind(release_id.0)
            .execute(&self.pool)
            .await
            .map_err(ManagerError::from)?
            .rows_affected()
        };
        Ok(affected == 1)
    }

    /// Persist/refresh a per-organization template publication record. The
    /// UNIQUE(org, release, role) constraint makes duplicate publication
    /// converge rather than append.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_org_template_version(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
        role: Role,
        template_id: &str,
        version_id: &str,
        import_job_id: &str,
        status: &str,
        error_code: Option<&str>,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "INSERT INTO organization_template_versions
                (id, organization_id, release_id, role, coder_template_id,
                 coder_version_id, import_job_id, status, error_code)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)
             ON CONFLICT (organization_id, release_id, role)
             DO UPDATE SET coder_template_id = EXCLUDED.coder_template_id,
                           coder_version_id = EXCLUDED.coder_version_id,
                           import_job_id = EXCLUDED.import_job_id,
                           status = EXCLUDED.status,
                           error_code = EXCLUDED.error_code,
                           updated_at = now()",
        )
        .bind(uuid::Uuid::new_v4())
        .bind(organization_id.0)
        .bind(release_id.0)
        .bind(role.as_str())
        .bind(template_id)
        .bind(version_id)
        .bind(import_job_id)
        .bind(status)
        .bind(error_code)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// Read the persisted publication for (org, release, role).
    pub async fn get_org_template_version(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
        role: Role,
    ) -> Result<Option<OrgTemplateVersionRow>, ManagerError> {
        let row = sqlx::query_as::<_, OrgTemplateVersionRow>(
            "SELECT id, organization_id, release_id, role, coder_template_id,
                    coder_version_id, import_job_id, status, error_code, created_at, updated_at
               FROM organization_template_versions
              WHERE organization_id = $1 AND release_id = $2 AND role = $3",
        )
        .bind(organization_id.0)
        .bind(release_id.0)
        .bind(role.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row)
    }

    /// Bind an organization to an approved release (the release it is
    /// reconciled against).
    pub async fn bind_org_release(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await?;
        Self::bind_org_release_in_tx(&mut tx, organization_id, release_id).await?;
        tx.commit().await?;
        Ok(())
    }

    pub async fn bind_org_release_in_tx(
        tx: &mut sqlx::PgConnection,
        organization_id: OrganizationId,
        release_id: ReleaseId,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "INSERT INTO organization_release_bindings (organization_id, release_id)
             VALUES ($1, $2)
             ON CONFLICT (organization_id, release_id) DO NOTHING",
        )
        .bind(organization_id.0)
        .bind(release_id.0)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        sqlx::query(
            "INSERT INTO organization_current_releases (organization_id, release_id)
            VALUES ($1, $2) ON CONFLICT (organization_id)
            DO UPDATE SET release_id = EXCLUDED.release_id",
        )
        .bind(organization_id.0)
        .bind(release_id.0)
        .execute(&mut *tx)
        .await?;
        Ok(())
    }

    /// Read the release id an organization is bound to, if any.
    pub async fn get_org_binding(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Option<ReleaseId>, ManagerError> {
        let row: Option<(uuid::Uuid,)> = sqlx::query_as(
            "SELECT release_id FROM organization_current_releases
              WHERE organization_id = $1",
        )
        .bind(organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(|(id,)| ReleaseId::from_uuid(id)))
    }

    /// Trusted startup may approve a newly staged configured release, but
    /// never undo an operator's retirement or overwrite approval metadata.
    pub async fn approve_configured_release(
        &self,
        release_id: ReleaseId,
    ) -> Result<(), ManagerError> {
        sqlx::query("UPDATE template_releases SET status = 'approved', approved_at = now(), updated_at = now()
            WHERE id = $1 AND status = 'staged'")
            .bind(release_id.0).execute(&self.pool).await?;
        Ok(())
    }

    /// Insert or update an organization provisioner record.
    pub async fn upsert_provisioner(
        &self,
        id: ProvisionerId,
        organization_id: OrganizationId,
        coder_provisioner_ref: &str,
        secret_ref: Option<&str>,
        status: &str,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "INSERT INTO organization_provisioners
                (id, organization_id, coder_provisioner_ref, secret_ref, status, last_heartbeat_at)
             VALUES ($1, $2, $3, $4, $5, CASE WHEN $5 = 'ready' THEN now() ELSE NULL END)
             ON CONFLICT (id) DO UPDATE SET
                 coder_provisioner_ref = EXCLUDED.coder_provisioner_ref,
                 secret_ref = COALESCE(EXCLUDED.secret_ref, organization_provisioners.secret_ref),
                 status = EXCLUDED.status,
                 last_heartbeat_at = CASE WHEN EXCLUDED.status = 'ready' THEN now() ELSE organization_provisioners.last_heartbeat_at END,
                 updated_at = now()",
        )
        .bind(id.0)
        .bind(organization_id.0)
        .bind(coder_provisioner_ref)
        .bind(secret_ref)
        .bind(status)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// Set the provisioner status and heartbeat for an organization.
    pub async fn set_provisioner_status(
        &self,
        organization_id: OrganizationId,
        status: &str,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE organization_provisioners
                SET status = $1, last_heartbeat_at = CASE WHEN $1 = 'ready' THEN now() ELSE last_heartbeat_at END, updated_at = now()
              WHERE organization_id = $2",
        )
        .bind(status)
        .bind(organization_id.0)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Fetch an organization's provisioner record.
    pub async fn get_provisioner(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Option<OrgProvisionerRow>, ManagerError> {
        let row = sqlx::query_as::<_, OrgProvisionerRow>(
            "SELECT id, organization_id, backend_resource_id, coder_provisioner_ref,
                    secret_ref, status, last_heartbeat_at
               FROM organization_provisioners
              WHERE organization_id = $1",
        )
        .bind(organization_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row)
    }
}

fn translate_release_insert(e: sqlx::Error, version: &str) -> ManagerError {
    if let sqlx::Error::Database(db) = &e {
        if db.constraint() == Some("template_releases_version_key") {
            return ManagerError::Conflict(format!(
                "template release version already exists: {version}"
            ));
        }
        if db.constraint() == Some("template_releases_manifest_sha256_key") {
            return ManagerError::Conflict(
                "template release with this manifest digest already exists".into(),
            );
        }
    }
    ManagerError::from(e)
}
