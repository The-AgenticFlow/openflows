//! PostgreSQL persistence: connection pool, migration runner, and transaction
//! scoping helpers.
//!
//! The Manager owns its own PostgreSQL database (the Openflows control plane),
//! separate from Coder's database. Migrations are embedded at compile time and
//! applied before the Manager reports product readiness.

pub mod tx;

use crate::error::ManagerError;
use sqlx::postgres::{PgConnectOptions, PgPoolOptions};
use sqlx::{PgPool, Postgres};
use std::time::Duration;

/// The number of numbered migrations embedded from `migrations/`.
pub const MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!();

/// A thin wrapper around a [`sqlx::PgPool`] that owns connection lifecycle and
/// migration state for the Manager.
#[derive(Clone)]
pub struct Db {
    pool: PgPool,
}

impl Db {
    /// Connect to PostgreSQL using `database_url` and apply all pending
    /// migrations. Returns an error if the database is unreachable or any
    /// migration fails; callers must leave the Manager unready in that case.
    pub async fn connect(database_url: &str) -> Result<Self, ManagerError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect(database_url)
            .await
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("database connect failed: {e}")))?;

        let db = Db { pool };
        db.migrate().await?;
        Ok(db)
    }

    /// Create a pool without applying migrations. Used by tests that manage
    /// their own migration lifecycle.
    pub async fn connect_unmigrated(database_url: &str) -> Result<Self, ManagerError> {
        let pool = PgPoolOptions::new()
            .max_connections(10)
            .acquire_timeout(Duration::from_secs(5))
            .connect(database_url)
            .await
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("database connect failed: {e}")))?;
        Ok(Db { pool })
    }

    /// Wrap an existing pool. Used by tests that build a pool with specific
    /// options (e.g. a lazy, unreachable pool to exercise readiness failures).
    pub fn from_pool(pool: PgPool) -> Self {
        Db { pool }
    }

    /// Apply all pending migrations. Rerunning against an already-migrated
    /// database is safe: sqlx records applied migrations in its bookkeeping
    /// table and applies only the ones not yet recorded.
    pub async fn migrate(&self) -> Result<(), ManagerError> {
        MIGRATOR
            .run(&self.pool)
            .await
            .map_err(|e| ManagerError::Migration(e.to_string()))
    }

    /// Number of applied migrations, for diagnostics and tests.
    pub async fn applied_migrations(&self) -> Result<i64, ManagerError> {
        let row: (i64,) =
            sqlx::query_as("SELECT count(*) FROM _sqlx_migrations WHERE success = true")
                .fetch_one(&self.pool)
                .await
                .map_err(ManagerError::from)?;
        Ok(row.0)
    }

    pub fn pool(&self) -> &PgPool {
        &self.pool
    }

    pub async fn ping(&self) -> Result<(), ManagerError> {
        sqlx::query("SELECT 1")
            .execute(&self.pool)
            .await
            .map_err(|e| ManagerError::Service(anyhow::anyhow!("database ping failed: {e}")))?;
        Ok(())
    }

    /// Acquire a transaction. Callers must use the [`tx::TxScope`] helpers to
    /// keep organization scope explicit.
    pub async fn begin(&self) -> Result<sqlx::Transaction<'_, Postgres>, ManagerError> {
        self.pool.begin().await.map_err(ManagerError::from)
    }
}

/// Parse a `postgres://` connection URL into a [`PgConnectOptions`] for
/// environments that need to override individual options.
pub fn connect_options(database_url: &str) -> Result<PgConnectOptions, ManagerError> {
    database_url
        .parse::<PgConnectOptions>()
        .map_err(|e| ManagerError::Config(format!("invalid database URL: {e}")))
}

/// Connect using a [`PgConnectOptions`], used by tests that create databases
/// dynamically.
pub async fn connect_with_options(options: PgConnectOptions) -> Result<Db, ManagerError> {
    let pool = PgPoolOptions::new()
        .max_connections(10)
        .connect_with(options)
        .await
        .map_err(|e| ManagerError::Service(anyhow::anyhow!("database connect failed: {e}")))?;
    Ok(Db { pool })
}
