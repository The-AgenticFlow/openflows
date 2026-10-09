//! Shared DTOs (data-transfer objects) for Manager services.
//!
//! These mirror the API contracts in the specifications. DTOs used to read
//! records are always resolved through an explicit organization scope before
//! being returned; they never carry authorization on their own.

use crate::id::*;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fmt;

macro_rules! string_enum {
    ($name:ident { $( $variant:ident => $value:literal ),+ $(,)? }) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $( $variant ),+ }

        impl $name {
            pub fn as_str(self) -> &'static str {
                match self { $( Self::$variant => $value ),+ }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result { f.write_str(self.as_str()) }
        }

        impl std::str::FromStr for $name {
            type Err = String;
            fn from_str(value: &str) -> Result<Self, Self::Err> {
                match value { $( $value => Ok(Self::$variant), )+ _ => Err(format!("invalid {}: {value}", stringify!($name))) }
            }
        }
    };
}

string_enum!(MembershipRole { Admin => "admin", Developer => "developer", Viewer => "viewer" });
string_enum!(MembershipStatus { Active => "active", Suspended => "suspended", Removed => "removed" });

/// A user as returned by `/api/v1/me`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UserDto {
    pub id: UserId,
    pub display_name: String,
    pub status: String,
}

/// A membership of a user in an organization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MembershipDto {
    pub organization_id: OrganizationId,
    pub role: MembershipRole,
    pub status: MembershipStatus,
}

/// An organization as returned to members. `coder_organization_id` is an opaque
/// upstream id and is only present once provisioning has assigned it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OrganizationDto {
    pub id: OrganizationId,
    pub slug: String,
    pub display_name: String,
    pub status: String,
    pub owner_user_id: UserId,
    pub coder_organization_id: Option<CoderId>,
}

/// A tenant within its organization.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TenantDto {
    pub id: TenantId,
    pub organization_id: OrganizationId,
    pub slug: String,
    pub github_repository_id: GithubId,
    pub desired_state: String,
    pub observed_state: String,
    pub fleet_size: i32,
}

/// An operation within its organization (sanitized: no raw upstream bodies).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OperationDto {
    pub id: OperationId,
    pub organization_id: OrganizationId,
    pub kind: String,
    pub state: String,
    pub current_step: Option<String>,
    pub attempt_count: i32,
    pub error_code: Option<String>,
}

/// A scoped cursor page for list endpoints.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CursorPage<T> {
    pub items: Vec<T>,
    pub next_cursor: Option<String>,
}

/// The serialized shape of an idempotency-record response reference.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct IdempotencyResponseRef {
    pub resource_type: String,
    pub resource_id: String,
    pub operation_id: Option<String>,
    pub created_at: DateTime<Utc>,
}
