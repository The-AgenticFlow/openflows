//! Central organization policy layer.
//!
//! One policy layer is used by every organization handler and service. It
//! resolves the caller's *current* membership and organization status from the
//! database (never from cached bearer claims or headers) and returns the
//! correct HTTP semantics:
//!
//!   * 401  – invalid/expired/revoked authentication, or the authenticated
//!     user is suspended.
//!   * 403  – an active member lacks the required role for the action.
//!   * 404  – the organization or resource does not exist or is outside the
//!     caller's membership; IDs are never enumerated.
//!
//! Ownership is a separate field on the organization, not a fourth role. Only
//! active admins may manage GitHub connections (enforced here even though the
//! connection lifecycle itself is WP-03). An `OrgScope` is a database scope,
//! never proof of authorization.

use crate::dto::{MembershipRole, MembershipStatus};
use crate::error::ManagerError;
use crate::id::{OrganizationId, UserId};

/// The caller's current membership in an organization, as read from the
/// database on every authorized request.
#[derive(Debug, Clone)]
pub struct Membership {
    pub organization_id: OrganizationId,
    pub user_id: UserId,
    pub role: MembershipRole,
    pub status: MembershipStatus,
}

/// The organization's current status and owner, read from the database.
#[derive(Debug, Clone)]
pub struct OrganizationState {
    pub organization_id: OrganizationId,
    pub status: String,
    pub owner_user_id: UserId,
}

/// The result of resolving a member against an organization.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MemberResolution {
    /// Active member with a role.
    Active,
    /// The member row exists but is suspended (they can still see their own
    /// limited context but cannot perform member actions).
    Suspended,
}

/// The policy layer. Pure and stateless; callers fetch current membership and
/// organization state then ask this layer for the decision.
#[derive(Debug, Default)]
pub struct Policy;

impl Policy {
    pub fn require_mutable(org: Option<&OrganizationState>) -> Result<(), ManagerError> {
        if org.is_some_and(|o| o.status == "ready" || o.status == "provisioning") {
            Ok(())
        } else {
            Err(ManagerError::api(
                "ORG_UNAVAILABLE",
                "organization does not accept mutations",
            ))
        }
    }

    /// Require an active membership. Returns 401 when the user is unknown or
    /// suspended, 404 when the organization is absent/outside membership.
    ///
    /// `membership` is `None` when the user has no membership row in this
    /// organization. `org` is `None` when the organization does not exist.
    pub fn require_member(
        membership: Option<&Membership>,
        org: Option<&OrganizationState>,
    ) -> Result<MemberResolution, ManagerError> {
        // An organization that exists but is not visible to the caller must be
        // reported as 404 (never as 403), so resource IDs are not enumerated.
        if org.is_none() {
            return Err(ManagerError::not_found("organization"));
        }
        let Some(m) = membership else {
            return Err(ManagerError::not_found("organization"));
        };
        if org.is_some_and(|o| o.organization_id != m.organization_id || o.status == "deleted") {
            return Err(ManagerError::not_found("organization"));
        }
        if org.is_some_and(|o| o.status == "suspended") {
            return Err(ManagerError::api(
                "ORG_UNAVAILABLE",
                "organization is suspended",
            ));
        }
        match m.status {
            MembershipStatus::Active => Ok(MemberResolution::Active),
            MembershipStatus::Suspended => Err(ManagerError::api(
                "MEMBER_SUSPENDED",
                "your membership in this organization is suspended",
            )),
            MembershipStatus::Removed => Err(ManagerError::not_found("organization")),
        }
    }

    /// Require an active admin. A suspended membership is denied; a viewer or
    /// developer is denied with 403.
    pub fn require_admin(
        membership: Option<&Membership>,
        org: Option<&OrganizationState>,
    ) -> Result<(), ManagerError> {
        Self::require_member(membership, org)?;
        Self::require_mutable(org)?;
        let m = membership.expect("require_member guarantees membership");
        match (m.status, m.role) {
            (MembershipStatus::Active, MembershipRole::Admin) => Ok(()),
            (MembershipStatus::Active, _) => Err(ManagerError::api(
                "ORG_ADMIN_REQUIRED",
                "an organization admin is required for this action",
            )),
            _ => Err(ManagerError::api(
                "ORG_ADMIN_REQUIRED",
                "an organization admin is required for this action",
            )),
        }
    }

    /// Require an active developer-or-above (admin or developer). Viewers are
    /// denied with 403.
    pub fn require_developer(
        membership: Option<&Membership>,
        org: Option<&OrganizationState>,
    ) -> Result<(), ManagerError> {
        Self::require_member(membership, org)?;
        Self::require_mutable(org)?;
        let m = membership.expect("require_member guarantees membership");
        if m.status != MembershipStatus::Active {
            return Err(ManagerError::api(
                "FORBIDDEN",
                "an active developer or admin is required for this action",
            ));
        }
        match m.role {
            MembershipRole::Admin | MembershipRole::Developer => Ok(()),
            MembershipRole::Viewer => Err(ManagerError::api(
                "FORBIDDEN",
                "a developer or admin is required for this action",
            )),
        }
    }

    /// Require the caller be the current owner of the organization. Ownership
    /// does not grant GitHub-connection rights; this check is only for the
    /// owner-only actions (ownership transfer, deletion request).
    pub fn require_owner(
        membership: Option<&Membership>,
        org: Option<&OrganizationState>,
    ) -> Result<(), ManagerError> {
        Self::require_member(membership, org)?;
        let Some(state) = org else {
            return Err(ManagerError::not_found("organization"));
        };
        let m = membership.expect("require_member guarantees membership");
        if m.status != MembershipStatus::Active {
            return Err(ManagerError::api(
                "OWNER_REQUIRED",
                "an active owner is required for this action",
            ));
        }
        if m.user_id != state.owner_user_id {
            return Err(ManagerError::api(
                "OWNER_REQUIRED",
                "only the organization owner can perform this action",
            ));
        }
        Ok(())
    }
}
