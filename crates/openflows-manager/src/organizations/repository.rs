//! Organization, membership, and invitation repository with organization-row
//! locking for member mutations and ownership transfer.
//!
//! Security rules enforced here (in addition to the policy layer):
//!   * Every lookup is joined to the caller's organization scope.
//!   * Member mutations and ownership transfer lock the organization row
//!     (`FOR UPDATE`) so concurrent demotions cannot remove the last active
//!     admin and the owner cannot be removed/suspended without transfer.
//!   * Invitation acceptance consumes the invitation atomically and requires
//!     the exact authenticated GitHub identity recorded on the invitation.

use crate::db::tx::OrgScope;
use crate::dto::{MembershipRole, MembershipStatus};
use crate::error::ManagerError;
use crate::id::{InvitationId, OrganizationId, UserId};
use crate::organizations::policy::{Membership, OrganizationState};
use chrono::{DateTime, Utc};
use sqlx::PgPool;

/// A persisted invitation row.
#[derive(Debug, Clone)]
pub struct InvitationRow {
    pub id: InvitationId,
    pub organization_id: OrganizationId,
    pub invitee_github_user_id: i64,
    pub role: MembershipRole,
    pub expires_at: DateTime<Utc>,
    pub accepted_at: Option<DateTime<Utc>>,
    pub revoked_at: Option<DateTime<Utc>>,
}

#[derive(Clone)]
pub struct OrganizationRepository {
    pool: PgPool,
}

impl OrganizationRepository {
    pub fn new(pool: PgPool) -> Self {
        OrganizationRepository { pool }
    }

    /// Fetch the caller's current membership, scoped to an organization that
    /// exists. Returns `(membership, org_state)`; both `None` when the
    /// organization does not exist or the caller has no membership.
    pub async fn membership_and_org(
        &self,
        org_id: OrganizationId,
        user_id: UserId,
    ) -> Result<(Option<Membership>, Option<OrganizationState>), ManagerError> {
        let row: Option<(String, uuid::Uuid, String, String)> = sqlx::query_as(
            "SELECT o.status,o.owner_user_id,m.role,m.status FROM organizations o
             JOIN memberships m ON m.organization_id=o.id
             JOIN users u ON u.id=m.user_id
             WHERE o.id=$1 AND m.user_id=$2 AND u.status='active'",
        )
        .bind(org_id.0)
        .bind(user_id.0)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        match row {
            None => Ok((None, None)),
            Some((org_status, owner, role, status)) => Ok((
                Some(Membership {
                    organization_id: org_id,
                    user_id,
                    role: role.parse().map_err(|_| {
                        ManagerError::Service(anyhow::anyhow!("invalid role in db"))
                    })?,
                    status: status.parse().map_err(|_| {
                        ManagerError::Service(anyhow::anyhow!("invalid status in db"))
                    })?,
                }),
                Some(OrganizationState {
                    organization_id: org_id,
                    status: org_status,
                    owner_user_id: UserId::from_uuid(owner),
                }),
            )),
        }
    }

    /// Lock organization first, then read authorization on this same connection.
    /// Every organization mutation follows this lock order.
    pub async fn lock_policy(
        &self,
        tx: &mut sqlx::PgConnection,
        org_id: OrganizationId,
        caller: UserId,
    ) -> Result<(Option<Membership>, Option<OrganizationState>), ManagerError> {
        let org: Option<(String, uuid::Uuid)> = sqlx::query_as(
            "SELECT status, owner_user_id FROM organizations WHERE id=$1 FOR UPDATE",
        )
        .bind(org_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        let member: Option<(String, String)> = sqlx::query_as(
            "SELECT m.role,m.status FROM memberships m JOIN users u ON u.id=m.user_id
             WHERE m.organization_id=$1 AND m.user_id=$2 AND u.status='active'",
        )
        .bind(org_id.0)
        .bind(caller.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        let membership = member
            .map(|(role, status)| -> Result<_, ManagerError> {
                Ok(Membership {
                    organization_id: org_id,
                    user_id: caller,
                    role: role
                        .parse()
                        .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid role")))?,
                    status: status.parse().map_err(|_| {
                        ManagerError::Service(anyhow::anyhow!("invalid membership status"))
                    })?,
                })
            })
            .transpose()?;
        Ok((
            membership,
            org.map(|(status, owner)| OrganizationState {
                organization_id: org_id,
                status,
                owner_user_id: UserId::from_uuid(owner),
            }),
        ))
    }

    /// List members of an organization (active, suspended, and removed), joined
    /// to the organization scope.
    pub async fn list_members(
        &self,
        scope: OrgScope,
        limit: i64,
        after_user: Option<uuid::Uuid>,
    ) -> Result<Vec<(UserId, String, String, String)>, ManagerError> {
        // Simple keyset over user_id; limited result size.
        let rows = match after_user {
            Some(after) => sqlx::query_as::<_, (uuid::Uuid, String, String, String)>(
                "SELECT m.user_id, u.display_name, m.role, m.status
                       FROM memberships m
                       JOIN users u ON u.id = m.user_id
                      WHERE m.organization_id = $1 AND m.user_id > $2
                      ORDER BY m.user_id
                      LIMIT $3",
            )
            .bind(scope.organization_id.0)
            .bind(after)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(ManagerError::from)?,
            None => sqlx::query_as::<_, (uuid::Uuid, String, String, String)>(
                "SELECT m.user_id, u.display_name, m.role, m.status
                       FROM memberships m
                       JOIN users u ON u.id = m.user_id
                      WHERE m.organization_id = $1
                      ORDER BY m.user_id
                      LIMIT $2",
            )
            .bind(scope.organization_id.0)
            .bind(limit)
            .fetch_all(&self.pool)
            .await
            .map_err(ManagerError::from)?,
        };
        Ok(rows
            .into_iter()
            .map(|(id, name, role, status)| (UserId::from_uuid(id), name, role, status))
            .collect())
    }

    /// Update a member's role/status with organization-row locking. Enforces:
    ///   * the owner must remain an active member (cannot be suspended/removed
    ///     without first transferring ownership);
    ///   * at least one active admin must remain (a demotion that would leave
    ///     zero active admins is rejected).
    ///
    /// Returns `Err` with the specific conflict when an invariant would be
    /// violated.
    pub async fn update_member(
        &self,
        scope: OrgScope,
        target_user: UserId,
        new_role: MembershipRole,
        new_status: MembershipStatus,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;
        self.update_member_in_tx(&mut tx, scope, target_user, new_role, new_status)
            .await?;
        tx.commit().await.map_err(ManagerError::from)?;
        Ok(())
    }

    pub async fn update_member_in_tx(
        &self,
        tx: &mut sqlx::PgConnection,
        scope: OrgScope,
        target_user: UserId,
        new_role: MembershipRole,
        new_status: MembershipStatus,
    ) -> Result<(), ManagerError> {
        // Lock the organization row to serialize membership mutations.
        let org: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT owner_user_id FROM organizations WHERE id = $1 FOR UPDATE")
                .bind(scope.organization_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(ManagerError::from)?;
        let Some((owner,)) = org else {
            return Err(ManagerError::not_found("organization"));
        };

        // The target must exist in this org.
        let target: Option<(String, String)> = sqlx::query_as(
            "SELECT role, status FROM memberships
              WHERE organization_id = $1 AND user_id = $2 FOR UPDATE",
        )
        .bind(scope.organization_id.0)
        .bind(target_user.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        let Some((target_role, target_status)) = target else {
            return Err(ManagerError::not_found("member"));
        };

        // Ownership requires active membership, not the admin role.
        if owner == target_user.0 && new_status != MembershipStatus::Active {
            return Err(ManagerError::Conflict(
                "transfer ownership before removing or suspending the owner".into(),
            ));
        }

        // Last-admin invariant: count active admins excluding this target, and
        // reject a change that would leave zero active admins.
        let is_current_admin = target_role == "admin" && target_status == "active";
        let becomes_active_admin =
            new_role == MembershipRole::Admin && new_status == MembershipStatus::Active;
        if is_current_admin && !becomes_active_admin {
            let other_admins: (i64,) = sqlx::query_as(
                "SELECT count(*) FROM memberships m JOIN users u ON u.id=m.user_id
                  WHERE m.organization_id = $1 AND m.role = 'admin' AND m.status = 'active'
                    AND m.user_id <> $2 AND u.status='active'",
            )
            .bind(scope.organization_id.0)
            .bind(target_user.0)
            .fetch_one(&mut *tx)
            .await
            .map_err(ManagerError::from)?;
            if other_admins.0 == 0 {
                return Err(ManagerError::Conflict(
                    "cannot demote, suspend, or remove the last active admin".to_string(),
                ));
            }
        }

        sqlx::query(
            "UPDATE memberships
                SET role = $1, status = $2, updated_at = now()
              WHERE organization_id = $3 AND user_id = $4",
        )
        .bind(new_role.as_str())
        .bind(new_status.as_str())
        .bind(scope.organization_id.0)
        .bind(target_user.0)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        Ok(())
    }

    /// Remove a member (mark removed) with the same invariants as `update_member`.
    pub async fn remove_member(
        &self,
        scope: OrgScope,
        target_user: UserId,
    ) -> Result<(), ManagerError> {
        self.update_member(
            scope,
            target_user,
            MembershipRole::Viewer,
            MembershipStatus::Removed,
        )
        .await
    }

    /// Transfer ownership to `new_owner` (must be an active member) with
    /// organization-row locking. The previous owner remains an admin unless
    /// separately changed.
    pub async fn transfer_ownership(
        &self,
        scope: OrgScope,
        new_owner: UserId,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;
        self.transfer_ownership_in_tx(&mut tx, scope, new_owner)
            .await?;
        tx.commit().await.map_err(ManagerError::from)?;
        Ok(())
    }

    pub async fn transfer_ownership_in_tx(
        &self,
        tx: &mut sqlx::PgConnection,
        scope: OrgScope,
        new_owner: UserId,
    ) -> Result<(), ManagerError> {
        // Lock the organization row.
        let org: Option<(uuid::Uuid,)> =
            sqlx::query_as("SELECT owner_user_id FROM organizations WHERE id = $1 FOR UPDATE")
                .bind(scope.organization_id.0)
                .fetch_optional(&mut *tx)
                .await
                .map_err(ManagerError::from)?;
        let Some((current_owner,)) = org else {
            return Err(ManagerError::not_found("organization"));
        };

        // The new owner must be an active member.
        let new_member: Option<(String,)> = sqlx::query_as(
            "SELECT m.status FROM memberships m JOIN users u ON u.id=m.user_id
              WHERE m.organization_id = $1 AND m.user_id = $2 AND u.status='active' FOR UPDATE OF m",
        )
        .bind(scope.organization_id.0)
        .bind(new_owner.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        let Some((status,)) = new_member else {
            return Err(ManagerError::not_found("member"));
        };
        if status != "active" {
            return Err(ManagerError::Conflict(
                "the new owner must be an active member".to_string(),
            ));
        }

        sqlx::query(
            "UPDATE organizations SET owner_user_id = $1, updated_at = now()
              WHERE id = $2",
        )
        .bind(new_owner.0)
        .bind(scope.organization_id.0)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        let _ = current_owner;
        Ok(())
    }

    /// Create an invitation, safely replacing an expired or revoked live
    /// invitation for the same org/subject. Enforces one live invitation per
    /// org/subject; returns 409 when a live (unexpired) invitation already
    /// exists.
    pub async fn create_invitation(
        &self,
        scope: OrgScope,
        invitee_github_user_id: i64,
        role: MembershipRole,
        token_hash: &str,
        invited_by: UserId,
    ) -> Result<InvitationId, ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;

        let id = self
            .create_invitation_in_tx(
                &mut tx,
                scope,
                invitee_github_user_id,
                role,
                token_hash,
                invited_by,
            )
            .await?;
        tx.commit().await.map_err(ManagerError::from)?;
        Ok(id)
    }

    pub async fn create_invitation_in_tx(
        &self,
        tx: &mut sqlx::PgConnection,
        scope: OrgScope,
        invitee_github_user_id: i64,
        role: MembershipRole,
        token_hash: &str,
        invited_by: UserId,
    ) -> Result<InvitationId, ManagerError> {
        // Revoke any expired/live invitation for the same org/subject so a
        // replacement is safe (safe replacement of expired invitations).
        sqlx::query(
            "UPDATE invitations SET revoked_at = now()
              WHERE organization_id = $1 AND invitee_github_user_id = $2
                AND accepted_at IS NULL AND revoked_at IS NULL AND expires_at <= clock_timestamp()",
        )
        .bind(scope.organization_id.0)
        .bind(invitee_github_user_id)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        // Enforce one live invitation per org/subject even under concurrency:
        // the partial unique index does this at the database level.
        let id = InvitationId::new();
        sqlx::query(
            "INSERT INTO invitations
                (id, organization_id, invitee_github_user_id, role, token_hash, invited_by, expires_at)
             VALUES ($1, $2, $3, $4, $5, $6, clock_timestamp() + interval '7 days')",
        )
        .bind(id.0)
        .bind(scope.organization_id.0)
        .bind(invitee_github_user_id)
        .bind(role.as_str())
        .bind(token_hash)
        .bind(invited_by.0)
        .execute(&mut *tx)
        .await
        .map_err(translate_invite_insert)?;

        Ok(id)
    }

    /// Revoke a live invitation by id within the org scope.
    pub async fn revoke_invitation(
        &self,
        scope: OrgScope,
        id: InvitationId,
    ) -> Result<bool, ManagerError> {
        let mut conn = self.pool.acquire().await.map_err(ManagerError::from)?;
        self.revoke_invitation_in_tx(&mut conn, scope, id).await
    }

    pub async fn revoke_invitation_in_tx(
        &self,
        tx: &mut sqlx::PgConnection,
        scope: OrgScope,
        id: InvitationId,
    ) -> Result<bool, ManagerError> {
        let affected = sqlx::query(
            "UPDATE invitations SET revoked_at = now()
              WHERE id = $1 AND organization_id = $2 AND accepted_at IS NULL AND revoked_at IS NULL",
        )
        .bind(id.0)
        .bind(scope.organization_id.0)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        Ok(affected == 1)
    }

    /// Look up a live (not accepted/revoked/expired) invitation by token hash,
    /// with its organization and role. The invitation URL is the delivery
    /// mechanism; acceptance additionally requires the exact GitHub identity.
    pub async fn invitation_by_token(
        &self,
        token_hash: &str,
    ) -> Result<Option<InvitationRow>, ManagerError> {
        let row = sqlx::query_as::<
            _,
            (uuid::Uuid, uuid::Uuid, i64, String, DateTime<Utc>, Option<DateTime<Utc>>, Option<DateTime<Utc>>),
        >(
            "SELECT id, organization_id, invitee_github_user_id, role, expires_at, accepted_at, revoked_at
               FROM invitations
              WHERE token_hash = $1",
        )
        .bind(token_hash)
        .fetch_optional(&self.pool)
        .await
        .map_err(ManagerError::from)?;
        Ok(row.map(
            |(id, org, invitee, role, expires, accepted, revoked)| InvitationRow {
                id: InvitationId::from_uuid(id),
                organization_id: OrganizationId::from_uuid(org),
                invitee_github_user_id: invitee,
                role: role
                    .parse::<MembershipRole>()
                    .map_err(|_| ManagerError::Service(anyhow::anyhow!("invalid role in db")))
                    .ok()
                    .unwrap_or(MembershipRole::Viewer),
                expires_at: expires,
                accepted_at: accepted,
                revoked_at: revoked,
            },
        ))
    }

    /// Accept an invitation: verify the exact authenticated GitHub identity,
    /// expiry, and single consumption; then add the membership in one
    /// transaction.
    ///
    /// Single consumption is enforced by a conditional `UPDATE ... WHERE
    /// accepted_at IS NULL AND revoked_at IS NULL`. This UPDATE is deliberately
    /// the first statement to touch the row and is the atomic gate: concurrent
    /// Updates on the same row serialize in PostgreSQL, and every loser
    /// re-evaluates the WHERE against the committed row and affects zero rows.
    pub async fn accept_invitation(
        &self,
        invitation_id: InvitationId,
        auth_github_user_id: i64,
        user_id: UserId,
    ) -> Result<(), ManagerError> {
        let mut tx = self.pool.begin().await.map_err(ManagerError::from)?;

        self.accept_invitation_in_tx(&mut tx, invitation_id, auth_github_user_id, user_id)
            .await?;
        tx.commit().await.map_err(ManagerError::from)?;
        Ok(())
    }

    pub async fn accept_invitation_in_tx(
        &self,
        tx: &mut sqlx::PgConnection,
        invitation_id: InvitationId,
        auth_github_user_id: i64,
        user_id: UserId,
    ) -> Result<(), ManagerError> {
        // Read invitation data WITHOUT locking so the conditional UPDATE below
        // is the sole atomic consumption gate.
        let row = sqlx::query_as::<
            _,
            (uuid::Uuid, i64, String, DateTime<Utc>, Option<DateTime<Utc>>, Option<DateTime<Utc>>),
        >(
            "SELECT organization_id, invitee_github_user_id, role, expires_at, accepted_at, revoked_at
               FROM invitations WHERE id = $1",
        )
        .bind(invitation_id.0)
        .fetch_optional(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        let Some((org_id, invitee_id, role, expires_at, _accepted_at, _revoked_at)) = row else {
            return Err(ManagerError::not_found("invitation"));
        };

        let identity_ok: bool = sqlx::query_scalar(
            "SELECT EXISTS(SELECT 1 FROM identities i JOIN users u ON u.id=i.user_id
             WHERE i.user_id=$1 AND i.provider='github' AND i.subject=$2 AND u.status='active')",
        )
        .bind(user_id.0)
        .bind(auth_github_user_id.to_string())
        .fetch_one(&mut *tx)
        .await
        .map_err(ManagerError::from)?;
        if !identity_ok {
            return Err(ManagerError::api(
                "UNAUTHORIZED",
                "active GitHub identity required",
            ));
        }
        // Only the exact authenticated GitHub identity may accept.
        if invitee_id != auth_github_user_id {
            return Err(ManagerError::api(
                "INVITATION_WRONG_USER",
                "this invitation is not for your GitHub identity",
            ));
        }

        // Expiry (enforced again atomically in the UPDATE's WHERE too).
        if expires_at <= Utc::now() {
            return Err(ManagerError::Conflict("invitation expired".to_string()));
        }

        // Atomic single consumption: exactly one concurrent acceptor matches
        // zero- or one-row semantics such that only one can set accepted_at.
        let consumed = sqlx::query(
            "UPDATE invitations SET accepted_at = now()
              WHERE id = $1 AND accepted_at IS NULL AND revoked_at IS NULL
                AND expires_at > now()",
        )
        .bind(invitation_id.0)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?
        .rows_affected();
        if consumed == 0 {
            return Err(ManagerError::Conflict(
                "invitation already used".to_string(),
            ));
        }

        let inserted = sqlx::query(
            "INSERT INTO memberships (organization_id, user_id, role, status)
             VALUES ($1, $2, $3, 'active')
             ON CONFLICT (organization_id, user_id) DO NOTHING",
        )
        .bind(org_id)
        .bind(user_id.0)
        .bind(&role)
        .execute(&mut *tx)
        .await
        .map_err(ManagerError::from)?;

        if inserted.rows_affected() != 1 {
            return Err(ManagerError::Conflict(
                "membership already exists; an admin must change its role or status".into(),
            ));
        }
        Ok(())
    }
}

/// Translate an invitation insert failure, mapping the one-live-invitation
/// violation to a 409.
fn translate_invite_insert(e: sqlx::Error) -> ManagerError {
    if let sqlx::Error::Database(db) = &e {
        if db.constraint() == Some("one_live_invitation_per_org_subject") {
            return ManagerError::Conflict(
                "an active invitation already exists for this person".to_string(),
            );
        }
    }
    ManagerError::from(e)
}
