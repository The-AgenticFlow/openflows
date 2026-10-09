//! Typed identifiers for Openflows entities.
//!
//! Every entity has its own newtype over `Uuid` so a `TenantId` cannot be
//! accidentally passed where an `OrganizationId` is expected. This is a core
//! part of the WP-01 "resource IDs alone must not authorize access" boundary:
//! repository methods take an explicit organization scope alongside the
//! resource id and both are type-checked at compile time.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;
use uuid::Uuid;

macro_rules! typed_id {
    ($name:ident) => {
        #[derive(
            Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize,
        )]
        #[serde(transparent)]
        pub struct $name(pub Uuid);

        impl $name {
            pub fn new() -> Self {
                $name(Uuid::new_v4())
            }

            pub fn from_uuid(uuid: Uuid) -> Self {
                $name(uuid)
            }

            pub fn as_uuid(&self) -> Uuid {
                self.0
            }
        }

        impl Default for $name {
            fn default() -> Self {
                $name(Uuid::nil())
            }
        }

        impl FromStr for $name {
            type Err = uuid::Error;
            fn from_str(s: &str) -> Result<Self, Self::Err> {
                Ok($name(Uuid::from_str(s)?))
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}", self.0)
            }
        }

        impl From<Uuid> for $name {
            fn from(uuid: Uuid) -> Self {
                $name(uuid)
            }
        }

        impl From<$name> for Uuid {
            fn from(id: $name) -> Self {
                id.0
            }
        }
    };
}

typed_id!(UserId);
typed_id!(OrganizationId);
typed_id!(IdentityId);
typed_id!(InvitationId);
typed_id!(SessionId);
typed_id!(ConnectionId);
typed_id!(TenantId);
typed_id!(WorkspaceId);
typed_id!(RuntimeIdentityId);
typed_id!(OperationId);
typed_id!(ReleaseId);
typed_id!(WebhookDeliveryId);
typed_id!(AuditEventId);
typed_id!(OutboxEventId);
typed_id!(CredentialLeaseId);
typed_id!(ProvisionerId);

/// A GitHub numeric id (signed 64-bit per the shared conventions).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct GithubId(pub i64);

impl GithubId {
    pub fn new(id: i64) -> Self {
        GithubId(id)
    }
}

impl From<i64> for GithubId {
    fn from(id: i64) -> Self {
        GithubId(id)
    }
}

impl fmt::Display for GithubId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// An opaque upstream Coder identifier (never user-controlled).
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct CoderId(pub String);

impl CoderId {
    pub fn new(id: impl Into<String>) -> Self {
        CoderId(id.into())
    }
}

impl fmt::Display for CoderId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}
