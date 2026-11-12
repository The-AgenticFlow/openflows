//! Persistence for template releases, per-organization publication, and
//! organization provisioners.
//!
//! Reuses the existing schema (`template_releases`, `template_release_roles`,
//! `organization_template_versions`, `organization_provisioners`) plus the
//! organization release bindings. All reads are organization-scoped.

use crate::error::ManagerError;
use crate::id::{OrganizationId, ProvisionerId, ReleaseId, TenantId, WorkspaceId};
use chrono::{DateTime, Utc};
use sqlx::PgPool;
use std::collections::BTreeMap;

use super::backend::ProvisionerBackend;
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
    pub backend: String,
    pub role: String,
    pub archive_digest: String,
    pub parameter_schema_version: i32,
    pub architecture: String,
    pub image_repository: Option<String>,
    pub image_digest: Option<String>,
    pub workspace_runtime_release: Option<String>,
}

/// A per-organization template publication (`organization_template_versions`).
#[derive(Debug, Clone, sqlx::FromRow)]
pub struct OrgTemplateVersionRow {
    pub id: uuid::Uuid,
    pub organization_id: uuid::Uuid,
    pub release_id: uuid::Uuid,
    pub backend: String,
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

        let variants: Vec<&RoleManifestEntry> = if manifest.variants.is_empty() {
            manifest.roles.values().collect()
        } else {
            manifest.all_variants().map(|(_, entry)| entry).collect()
        };

        for entry in variants {
            sqlx::query(
                "INSERT INTO template_release_roles
                    (release_id, backend, role, archive_digest, parameter_schema_version,
                     architecture, image_repository, image_digest, workspace_runtime_release)
                 VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
            )
            .bind(release_id.0)
            .bind(entry.backend.as_str())
            .bind(entry.role.as_str())
            .bind(&entry.sha256)
            .bind(entry.parameter_schema_version as i32)
            .bind(&entry.architecture)
            .bind(&entry.image_repository)
            .bind(&entry.image_digest)
            .bind(&entry.workspace_runtime_release)
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

    /// Fetch a release and all its `(backend, role)` variants by id.
    pub async fn get_release_variants(
        &self,
        release_id: ReleaseId,
    ) -> Result<
        Option<(
            ReleaseRow,
            BTreeMap<(ProvisionerBackend, Role), RoleManifestEntry>,
        )>,
        ManagerError,
    > {
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
                let variants = self.load_variants(row.id).await?;
                Ok(Some((row, variants)))
            }
            None => Ok(None),
        }
    }

    /// Load all `(backend, role)` variants for a release.
    pub async fn load_variants(
        &self,
        release_id: uuid::Uuid,
    ) -> Result<BTreeMap<(ProvisionerBackend, Role), RoleManifestEntry>, ManagerError> {
        let rows = sqlx::query_as::<_, ReleaseRoleRow>(
            "SELECT release_id, backend, role, archive_digest, parameter_schema_version,
                    architecture, image_repository, image_digest, workspace_runtime_release
               FROM template_release_roles
              WHERE release_id = $1",
        )
        .bind(release_id)
        .fetch_all(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        let mut map = BTreeMap::new();
        for r in rows {
            let backend: ProvisionerBackend = r.backend.parse().map_err(|_| {
                ManagerError::Service(anyhow::anyhow!(
                    "unknown backend in database: {}",
                    r.backend
                ))
            })?;
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
                (backend, role),
                RoleManifestEntry {
                    role,
                    backend,
                    archive: role.archive_name_for_backend(backend),
                    sha256: r.archive_digest,
                    parameter_schema_version: r.parameter_schema_version as u32,
                    architecture: r.architecture,
                    image_repository: r.image_repository,
                    image_digest: r.image_digest,
                    workspace_runtime_release: r.workspace_runtime_release,
                },
            );
        }
        Ok(map)
    }

    async fn load_roles(
        &self,
        release_id: uuid::Uuid,
    ) -> Result<BTreeMap<Role, RoleManifestEntry>, ManagerError> {
        let variants = self.load_variants(release_id).await?;
        let mut map = BTreeMap::new();
        for ((backend, role), entry) in &variants {
            if *backend == ProvisionerBackend::Docker {
                map.insert(*role, entry.clone());
            }
        }
        if map.is_empty() {
            for ((_, role), entry) in variants {
                map.entry(role).or_insert(entry);
            }
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
    /// UNIQUE(org, release, backend, role) constraint makes duplicate
    /// publication converge rather than append.
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
        self.upsert_org_template_version_for_backend(
            organization_id,
            release_id,
            ProvisionerBackend::Docker,
            role,
            template_id,
            version_id,
            import_job_id,
            status,
            error_code,
        )
        .await
    }

    /// Persist/refresh a per-organization template publication record for a
    /// specific provisioner backend.
    #[allow(clippy::too_many_arguments)]
    pub async fn upsert_org_template_version_for_backend(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
        backend: ProvisionerBackend,
        role: Role,
        template_id: &str,
        version_id: &str,
        import_job_id: &str,
        status: &str,
        error_code: Option<&str>,
    ) -> Result<(), ManagerError> {
        sqlx::query(
            "INSERT INTO organization_template_versions
                (id, organization_id, release_id, backend, role, coder_template_id,
                 coder_version_id, import_job_id, status, error_code)
             VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9, $10)
             ON CONFLICT (organization_id, release_id, backend, role)
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
        .bind(backend.as_str())
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

    /// Read the persisted publication for (org, release, docker, role).
    pub async fn get_org_template_version(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
        role: Role,
    ) -> Result<Option<OrgTemplateVersionRow>, ManagerError> {
        self.get_org_template_version_for_backend(
            organization_id,
            release_id,
            ProvisionerBackend::Docker,
            role,
        )
        .await
    }

    /// Read the persisted publication for (org, release, backend, role).
    pub async fn get_org_template_version_for_backend(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
        backend: ProvisionerBackend,
        role: Role,
    ) -> Result<Option<OrgTemplateVersionRow>, ManagerError> {
        let row = sqlx::query_as::<_, OrgTemplateVersionRow>(
            "SELECT id, organization_id, release_id, backend, role, coder_template_id,
                    coder_version_id, import_job_id, status, error_code, created_at, updated_at
               FROM organization_template_versions
              WHERE organization_id = $1 AND release_id = $2 AND backend = $3 AND role = $4",
        )
        .bind(organization_id.0)
        .bind(release_id.0)
        .bind(backend.as_str())
        .bind(role.as_str())
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row)
    }

    /// Read a tenant's pinned release and backend, if set.
    pub async fn get_tenant_pin(
        &self,
        organization_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<Option<(Option<ReleaseId>, Option<ProvisionerBackend>)>, ManagerError> {
        let row: Option<(Option<uuid::Uuid>, Option<String>)> = sqlx::query_as(
            "SELECT pinned_release_id, pinned_backend
               FROM tenants
              WHERE organization_id = $1 AND id = $2",
        )
        .bind(organization_id.0)
        .bind(tenant_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        match row {
            None => Ok(None),
            Some((release_id, backend)) => {
                let parsed_backend = backend
                    .map(|b| {
                        b.parse::<ProvisionerBackend>().map_err(|_| {
                            ManagerError::Service(anyhow::anyhow!(
                                "unknown pinned_backend in database: {b}"
                            ))
                        })
                    })
                    .transpose()?;
                Ok(Some((release_id.map(ReleaseId::from_uuid), parsed_backend)))
            }
        }
    }

    /// Return the `pinned_backend` values for all workspaces belonging to `tenant_id`.
    pub async fn get_tenant_workspace_backends(
        &self,
        organization_id: OrganizationId,
        tenant_id: TenantId,
    ) -> Result<Vec<Option<ProvisionerBackend>>, ManagerError> {
        let rows: Vec<(Option<String>,)> = sqlx::query_as(
            "SELECT pinned_backend
               FROM workspaces
              WHERE organization_id = $1
                AND tenant_id = $2
                AND (
                    pinned_backend IS NOT NULL
                    OR pinned_version_id IS NOT NULL
                    OR pinned_release_id IS NOT NULL
                    OR coder_workspace_id IS NOT NULL
                )",
        )
        .bind(organization_id.0)
        .bind(tenant_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        rows.into_iter()
            .map(|(backend,)| {
                backend
                    .map(|b| {
                        b.parse::<ProvisionerBackend>().map_err(|_| {
                            ManagerError::Service(anyhow::anyhow!(
                                "unknown workspace pinned_backend in database: {b}"
                            ))
                        })
                    })
                    .transpose()
            })
            .collect()
    }

    /// Pin a tenant to a release and backend only if not already pinned.
    pub async fn set_tenant_pin_if_unset(
        &self,
        organization_id: OrganizationId,
        tenant_id: TenantId,
        release_id: ReleaseId,
        backend: ProvisionerBackend,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE tenants
                SET pinned_release_id = COALESCE(pinned_release_id, $3),
                    pinned_backend = COALESCE(pinned_backend, $4),
                    updated_at = now()
              WHERE organization_id = $1
                AND id = $2
                AND (pinned_release_id IS NULL OR $4::text = 'docker')
                AND (pinned_release_id IS NULL OR pinned_backend IS NULL)",
        )
        .bind(organization_id.0)
        .bind(tenant_id.0)
        .bind(release_id.0)
        .bind(backend.as_str())
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Pin a workspace to a release, backend, and optional Coder version id.
    pub async fn set_workspace_pin(
        &self,
        organization_id: OrganizationId,
        workspace_id: WorkspaceId,
        release_id: ReleaseId,
        backend: ProvisionerBackend,
        version_id: Option<&str>,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE workspaces
                SET pinned_release_id = $3,
                    pinned_backend = $4,
                    pinned_version_id = COALESCE($5, pinned_version_id),
                    updated_at = now()
              WHERE organization_id = $1
                AND id = $2
                AND (pinned_backend IS NULL OR pinned_backend = $4::text)
                AND (pinned_version_id IS NULL OR pinned_backend IS NOT NULL OR $4::text = 'docker')",
        )
        .bind(organization_id.0)
        .bind(workspace_id.0)
        .bind(release_id.0)
        .bind(backend.as_str())
        .bind(version_id)
        .execute(&self.pool)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Atomically verify/set the tenant pin and record the workspace pin in one
    /// transaction.
    ///
    /// Locks the tenant row with `FOR UPDATE` so concurrent provisioning
    /// requests cannot pin a workspace to an outdated release after another
    /// request has already pinned the tenant to a different release or backend.
    pub async fn record_workspace_and_tenant_pin(
        &self,
        organization_id: OrganizationId,
        tenant_id: TenantId,
        workspace_id: WorkspaceId,
        release_id: ReleaseId,
        backend: ProvisionerBackend,
        version_id: &str,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;
        let row: Option<(Option<uuid::Uuid>, Option<String>)> = sqlx::query_as(
            "SELECT pinned_release_id, pinned_backend
               FROM tenants
              WHERE organization_id = $1 AND id = $2
              FOR UPDATE",
        )
        .bind(organization_id.0)
        .bind(tenant_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        let Some((existing_release_uuid, existing_backend_str)) = row else {
            return Err(ManagerError::api(
                crate::provisioning::tenant_worker::err::RELEASE_NOT_RESOLVED,
                "tenant not found when recording release pin",
            ));
        };
        let existing_release = existing_release_uuid.map(ReleaseId::from_uuid);
        let existing_backend = existing_backend_str
            .map(|b| {
                b.parse::<ProvisionerBackend>().map_err(|_| {
                    ManagerError::Service(anyhow::anyhow!(
                        "unknown pinned_backend in database: {b}"
                    ))
                })
            })
            .transpose()?;

        // A legacy tenant with a release pin and NULL backend belongs to Docker.
        let effective_backend = existing_backend.or_else(|| {
            existing_release
                .is_some()
                .then_some(ProvisionerBackend::Docker)
        });
        if let Some(pinned_backend) = effective_backend {
            if pinned_backend != backend {
                return Err(ManagerError::api(
                    crate::provisioning::tenant_worker::err::RELEASE_NOT_RESOLVED,
                    format!(
                        "tenant is pinned to backend '{}' and cannot use '{}'",
                        pinned_backend, backend
                    ),
                ));
            }
        }
        if let Some(pinned_release) = existing_release {
            if pinned_release != release_id {
                return Err(ManagerError::api(
                    crate::provisioning::tenant_worker::err::RELEASE_NOT_RESOLVED,
                    "tenant is pinned to a different release",
                ));
            }
        }

        if existing_release.is_none() || existing_backend.is_none() {
            sqlx::query(
                "UPDATE tenants
                    SET pinned_release_id = COALESCE(pinned_release_id, $3),
                        pinned_backend = COALESCE(pinned_backend, $4),
                        updated_at = now()
                  WHERE organization_id = $1 AND id = $2",
            )
            .bind(organization_id.0)
            .bind(tenant_id.0)
            .bind(release_id.0)
            .bind(backend.as_str())
            .execute(&mut *tx)
            .await
            .map_err(ManagerError::from)?;
        }

        let affected = sqlx::query(
            "UPDATE workspaces
                SET pinned_release_id = $3,
                    pinned_backend = $4,
                    pinned_version_id = COALESCE($5, pinned_version_id),
                    updated_at = now()
              WHERE organization_id = $1
                AND id = $2
                AND (pinned_backend IS NULL OR pinned_backend = $4::text)
                AND (pinned_version_id IS NULL OR pinned_backend IS NOT NULL OR $4::text = 'docker')",
        )
        .bind(organization_id.0)
        .bind(workspace_id.0)
        .bind(release_id.0)
        .bind(backend.as_str())
        .bind(version_id)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        if affected != 1 {
            return Err(ManagerError::api(
                crate::provisioning::tenant_worker::err::RELEASE_NOT_RESOLVED,
                "workspace is pinned to a different backend",
            ));
        }

        tx.commit().await.map_err(ManagerError::from)?;
        Ok(())
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
