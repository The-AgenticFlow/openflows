//! Template release orchestration: operator-configured release import, per-org
//! publication, retry-safe convergence, and platform-only approval/promotion.

use crate::coder::{CoderAdapter, PublishOutcome, PublishedTemplate};
use crate::error::ManagerError;
use crate::id::{OrganizationId, ReleaseId};
use crate::templates::packaging::{Role, TemplateManifest};
use crate::templates::repository::ReleaseRepository;
use serde_json::json;
use sqlx::PgPool;

/// The role archives of a release as resolved from the operator-configured
/// artifact source (a role -> packaged archive bytes mapping).
#[derive(Clone, Debug)]
pub struct ReleaseArtifacts {
    pub manifest: TemplateManifest,
    /// role -> deterministic archive bytes
    pub archives: std::collections::BTreeMap<Role, Vec<u8>>,
}

#[derive(Clone)]
pub struct ReleaseService {
    pub pool: PgPool,
    pub repo: ReleaseRepository,
}

impl ReleaseService {
    pub fn new(pool: PgPool, repo: ReleaseRepository) -> Self {
        ReleaseService { pool, repo }
    }

    /// Import an operator-configured release: validate the manifest and each
    /// role archive's digest, then persist the immutable release records. The
    /// artifact source is operator-configured (never customer-supplied URLs).
    ///
    /// Returns the created release id and its manifest digest. Idempotent by
    /// digest: importing the same digest returns the existing release.
    pub async fn import_release(
        &self,
        artifacts: ReleaseArtifacts,
        artifact_uri: &str,
    ) -> Result<(ReleaseId, String), ManagerError> {
        // Validate manifest structure.
        let manifest = &artifacts.manifest;
        TemplateManifest::parse(&manifest.to_canonical_json()?)?;
        let digest = manifest.manifest_sha256()?;

        // Reject if this exact digest is already imported (immutable identity).
        if let Some((existing, _)) = self.repo.get_release_by_digest(&digest).await? {
            return Ok((ReleaseId::from_uuid(existing.id), digest));
        }

        // Validate every required role archive is present and matches its digest.
        for role in &manifest.required_roles {
            let entry = manifest
                .roles
                .get(role)
                .ok_or_else(|| ManagerError::InvalidInput("missing role entry".into()))?;
            let archive = artifacts.archives.get(role).ok_or_else(|| {
                ManagerError::InvalidInput(format!(
                    "artifact source did not provide archive for role '{}'",
                    role.as_str()
                ))
            })?;
            super::packaging::verify_archive_digest(archive, &entry.sha256)?;
            super::packaging::validate_archive(archive)?;
        }

        // A release label is an immutable operator-visible identity. Returning
        // an existing release is safe only when the complete manifest digest
        // matches; silently accepting different bytes here would let a
        // restarted deployment alter releases already bound to organizations.
        if let Some((existing, _)) = self.repo.get_release_by_version(&manifest.release).await? {
            if existing.manifest_sha256 == digest {
                return Ok((ReleaseId::from_uuid(existing.id), digest));
            }
            return Err(ManagerError::Conflict(format!(
                "configured template release '{}' differs from the immutable release already stored (stored manifest digest {}, configured manifest digest {}); publish the changed templates under a new OPENFLOWS_RELEASE_NAME",
                manifest.release, existing.manifest_sha256, digest
            )));
        }

        let release_id = ReleaseId::new();
        if let Err(error) = self
            .repo
            .insert_release(release_id, manifest, artifact_uri, &digest)
            .await
        {
            // A competing manager may have committed the same immutable
            // release while we validated it. Conflicting versions still fail.
            if matches!(error, ManagerError::Conflict(_)) {
                if let Some((existing, _)) = self.repo.get_release_by_digest(&digest).await? {
                    return Ok((ReleaseId::from_uuid(existing.id), digest));
                }
            }
            return Err(error);
        }
        Ok((release_id, digest))
    }

    /// Publish the required roles of `release_id` into `organization_id` using
    /// `adapter`. Each role is published idempotently (retry-safe) and its
    /// import must succeed before the publication is considered ready. The
    /// exact template/version/import-job IDs are persisted and verified.
    pub async fn publish_for_org(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
        adapter: &dyn CoderAdapter,
        artifacts: &ReleaseArtifacts,
    ) -> Result<Vec<Role>, ManagerError> {
        let (release, roles) = self
            .repo
            .get_release(release_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("template release"))?;

        TemplateManifest::parse(&artifacts.manifest.to_canonical_json()?)?;
        if artifacts.manifest.manifest_sha256()? != release.manifest_sha256 {
            return Err(ManagerError::InvalidInput(
                "manifest does not match persisted release".into(),
            ));
        }

        // The artifact bytes must be exactly the immutable release recorded in
        // the database. Never publish independently supplied bytes under an
        // existing release identity.
        for role in &artifacts.manifest.required_roles {
            let entry = roles
                .get(role)
                .ok_or_else(|| ManagerError::InvalidInput("missing role entry".into()))?;
            let archive = artifacts
                .archives
                .get(role)
                .ok_or_else(|| ManagerError::InvalidInput("missing role archive".into()))?;
            if crate::templates::packaging::sha256_hex(archive) != entry.sha256 {
                return Err(ManagerError::InvalidInput(format!(
                    "role '{}' archive does not match persisted release",
                    role.as_str()
                )));
            }
        }

        // Resolve the mapped Coder organization once.
        let coder_org = organization_coder_org(&self.pool, organization_id).await?;

        let mut published_roles = Vec::new();
        for role in &artifacts.manifest.required_roles {
            if !roles.contains_key(role) {
                return Err(ManagerError::InvalidInput("missing role entry".into()));
            }
            let archive = artifacts.archives.get(role).ok_or_else(|| {
                ManagerError::InvalidInput(format!(
                    "artifact source did not provide archive for role '{}'",
                    role.as_str()
                ))
            })?;

            let template_name = role.template_name();
            let version_name = release_id.to_string();
            let result = adapter
                .ensure_template_version(&coder_org, &template_name, &version_name, archive)
                .await?;

            let PublishedTemplate {
                template_id,
                version_id,
                import_job_id,
            } = result.published;

            // Persist the exact IDs. A failed import must not be recorded as
            // ready; the adapter already errors on failed/timed-out imports, so
            // reaching here means the import succeeded.
            self.repo
                .upsert_org_template_version(
                    organization_id,
                    release_id,
                    *role,
                    &template_id.0,
                    &version_id.0,
                    &import_job_id,
                    "ready",
                    None,
                )
                .await?;

            if result.outcome == PublishOutcome::Created {
                tracing::info!(
                    role = role.as_str(),
                    template_id = %template_id.0,
                    version_id = %version_id.0,
                    "published template role"
                );
            } else {
                tracing::info!(
                    role = role.as_str(),
                    version_id = %version_id.0,
                    "reconciled existing template role"
                );
            }
            published_roles.push(*role);
        }
        Ok(published_roles)
    }

    /// Platform-only approval: mark a staged release approved after its import
    /// and canary requirements are satisfied. The caller must have verified the
    /// operator is a platform operator (not an ordinary customer admin).
    pub async fn approve_release(
        &self,
        release_id: ReleaseId,
        approved_by: uuid::Uuid,
    ) -> Result<(), ManagerError> {
        let (row, _) = self
            .repo
            .get_release(release_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("template release"))?;
        if row.status == "approved" {
            return Ok(());
        }
        if row.status != "staged" {
            return Err(ManagerError::Conflict(
                "only a staged release can be approved".into(),
            ));
        }
        self.repo
            .set_release_status(release_id, "approved", Some(approved_by))
            .await?;
        Ok(())
    }

    /// Promote an approved release to an organization: bind the org to it and
    /// publish its roles. Promotion never silently upgrades existing pins; a
    /// separate explicit operation rebuilds pinned workspaces.
    pub async fn promote_to_org(
        &self,
        organization_id: OrganizationId,
        release_id: ReleaseId,
        adapter: &dyn CoderAdapter,
        artifacts: &ReleaseArtifacts,
    ) -> Result<(), ManagerError> {
        let (row, _) = self
            .repo
            .get_release(release_id)
            .await?
            .ok_or_else(|| ManagerError::not_found("template release"))?;
        if row.status != "approved" {
            return Err(ManagerError::Conflict(
                "only an approved release can be promoted".into(),
            ));
        }
        self.publish_for_org(organization_id, release_id, adapter, artifacts)
            .await?;
        let mut tx = self.pool.begin().await?;
        // Keep the lock order identical to the readiness worker: organization
        // first, then release. This prevents promotion/readiness deadlocks.
        sqlx::query("SELECT id FROM organizations WHERE id = $1 FOR UPDATE")
            .bind(organization_id.0)
            .fetch_one(&mut *tx)
            .await?;
        let approved: Option<uuid::Uuid> = sqlx::query_scalar(
            "SELECT id FROM template_releases WHERE id = $1 AND status = 'approved' FOR UPDATE",
        )
        .bind(release_id.0)
        .fetch_optional(&mut *tx)
        .await?;
        if approved.is_none() {
            return Err(ManagerError::Conflict(
                "release was retired while it was being published".into(),
            ));
        }
        ReleaseRepository::bind_org_release_in_tx(&mut tx, organization_id, release_id).await?;
        tx.commit().await?;
        Ok(())
    }

    /// Sanitized release publication progress for an organization (used by the
    /// operation / organization status interfaces).
    pub async fn org_publication_progress(
        &self,
        organization_id: OrganizationId,
    ) -> Result<Vec<serde_json::Value>, ManagerError> {
        let rows = sqlx::query_as::<_, crate::templates::repository::OrgTemplateVersionRow>(
            "SELECT id, organization_id, release_id, role, coder_template_id,
                    coder_version_id, import_job_id, status, error_code, created_at, updated_at
               FROM organization_template_versions
              WHERE organization_id = $1",
        )
        .bind(organization_id.0)
        .fetch_all(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(rows
            .into_iter()
            .map(|r| {
                json!({
                    "role": r.role,
                    "status": r.status,
                    "error_code": r.error_code,
                    "coder_template_id": r.coder_template_id,
                    "coder_version_id": r.coder_version_id,
                })
            })
            .collect())
    }
}

/// Publication uses the durable mapping; only provisioning may create it.
async fn organization_coder_org(
    pool: &PgPool,
    organization_id: OrganizationId,
) -> Result<coder_client::CoderOrganizationId, ManagerError> {
    let mapping: Option<Option<String>> =
        sqlx::query_scalar("SELECT coder_organization_id FROM organizations WHERE id = $1")
            .bind(organization_id.0)
            .fetch_optional(pool)
            .await?;
    mapping
        .flatten()
        .filter(|id| !id.is_empty())
        .map(coder_client::CoderOrganizationId::new)
        .ok_or_else(|| {
            ManagerError::api(
                "CODER_ORG_UNMAPPED",
                "organization requires provisioning reconciliation",
            )
        })
}

/// The deterministic, compact internal Coder organization name derived from
/// an Openflows organization id. UUID bytes are base32-encoded without
/// padding so the result stays within common Coder organization-name limits
/// while retaining all 128 bits of identity. Never customer-chosen.
pub fn internal_org_name(organization_id: OrganizationId) -> String {
    const BASE32: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";

    let mut encoded = String::with_capacity(26);
    let mut accumulator = 0u32;
    let mut bits = 0u8;
    for byte in organization_id.0.as_bytes() {
        accumulator = (accumulator << 8) | u32::from(*byte);
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            encoded.push(BASE32[((accumulator >> bits) & 0x1f) as usize] as char);
        }
    }
    if bits > 0 {
        encoded.push(BASE32[((accumulator << (5 - bits)) & 0x1f) as usize] as char);
    }

    format!("of-{encoded}")
}

#[cfg(test)]
mod tests {
    use super::internal_org_name;
    use crate::id::OrganizationId;
    use uuid::Uuid;

    #[test]
    fn coder_organization_name_is_compact_and_deterministic() {
        let id = OrganizationId::from_uuid(
            Uuid::parse_str("25725444-cf89-4edc-ab66-d4d9dd42e7d8").unwrap(),
        );
        let name = internal_org_name(id);

        assert_eq!(name, "of-evzfirgprfhnzk3g2tm52qxh3a");
        assert!(name.len() <= 32);
        assert!(name.chars().all(|character| character.is_ascii_lowercase()
            || character.is_ascii_digit()
            || character == '-'));
        assert_eq!(internal_org_name(id), name);
    }

    #[test]
    fn coder_organization_names_are_unique_for_distinct_ids() {
        let first = OrganizationId::from_uuid(Uuid::from_u128(1));
        let second = OrganizationId::from_uuid(Uuid::from_u128(2));

        assert_ne!(internal_org_name(first), internal_org_name(second));
    }
}
