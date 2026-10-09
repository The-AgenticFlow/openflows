//! Transaction scoping helpers.
//!
//! These helpers make organization/tenant scope explicit at the repository
//! boundary. A transaction is created with an [`OrgScope`] and every query run
//! through it carries that scope, so a resource id alone can never authorize a
//! cross-organization read or write.

use crate::error::ManagerError;
use crate::id::OrganizationId;
use sqlx::{Postgres, Transaction};

/// The organization scope a transaction operates under.
///
/// Repository methods take a scope and join every lookup to `organization_id`.
/// Passing a bare resource id without a scope is not possible through these
/// helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct OrgScope {
    pub organization_id: OrganizationId,
}

impl OrgScope {
    pub fn new(organization_id: OrganizationId) -> Self {
        OrgScope { organization_id }
    }
}

/// A transaction bound to an [`OrgScope`].
///
/// The type is a thin wrapper that keeps the scope attached so downstream
/// repository calls receive it explicitly rather than deriving it from a
/// resource id.
pub struct TxScope<'c> {
    pub scope: OrgScope,
    pub tx: Transaction<'c, Postgres>,
}

impl<'c> TxScope<'c> {
    pub fn new(scope: OrgScope, tx: Transaction<'c, Postgres>) -> Self {
        TxScope { scope, tx }
    }

    /// Commit the transaction.
    pub async fn commit(self) -> Result<(), ManagerError> {
        self.tx.commit().await.map_err(ManagerError::from)
    }

    /// Roll the transaction back (dropping also rolls back).
    pub async fn rollback(self) -> Result<(), ManagerError> {
        self.tx.rollback().await.map_err(ManagerError::from)
    }

    /// Access the underlying transaction for SQL execution.
    pub fn tx(&mut self) -> &mut Transaction<'c, Postgres> {
        &mut self.tx
    }
}

/// A connection-level execution guard that carries an org scope for queries
/// that should not run inside a transaction (e.g. read-only listing).
#[derive(Debug, Clone, Copy)]
pub struct Scoped<'c, C> {
    pub scope: OrgScope,
    pub conn: &'c C,
}

impl<'c, C> Scoped<'c, C> {
    pub fn new(scope: OrgScope, conn: &'c C) -> Self {
        Scoped { scope, conn }
    }
}
