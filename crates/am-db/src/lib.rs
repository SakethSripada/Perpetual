//! SQLite persistence layer for AgentManager (sqlx + runtime queries).
//!
//! Exposes a [`Db`] handle and a set of repository modules. The orchestrator
//! core depends on these functions rather than embedding SQL.

use std::path::Path;

use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions};
use sqlx::SqlitePool;

pub mod repos;

/// Errors surfaced by the persistence layer.
#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("database error: {0}")]
    Sqlx(#[from] sqlx::Error),
    #[error("migration error: {0}")]
    Migrate(#[from] sqlx::migrate::MigrateError),
    #[error("invalid stored enum value: {0}")]
    InvalidEnum(String),
    #[error("json error: {0}")]
    Serde(String),
    #[error("not found")]
    NotFound,
}

/// A connection pool plus the schema, ready to use.
#[derive(Clone)]
pub struct Db {
    pub pool: SqlitePool,
}

impl Db {
    /// Open (creating if needed) the SQLite database at `path`, applying all
    /// migrations. The parent directory must already exist.
    pub async fn connect(path: &Path) -> Result<Self, DbError> {
        // WAL lets readers proceed alongside the single writer; NORMAL sync is
        // durable-enough under WAL (a crash can lose the last transactions but
        // never corrupts). The busy timeout absorbs writer contention instead
        // of surfacing SQLITE_BUSY to hot paths, and the negative cache_size
        // is KiB of page cache per connection.
        let opts = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .foreign_keys(true)
            .journal_mode(SqliteJournalMode::Wal)
            .synchronous(sqlx::sqlite::SqliteSynchronous::Normal)
            .busy_timeout(std::time::Duration::from_secs(5))
            .pragma("cache_size", "-64000");

        let pool = SqlitePoolOptions::new()
            .max_connections(16)
            .connect_with(opts)
            .await?;

        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    /// In-memory database for tests.
    pub async fn connect_in_memory() -> Result<Self, DbError> {
        let opts = SqliteConnectOptions::new()
            .in_memory(true)
            .foreign_keys(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(opts)
            .await?;
        Self::migrate(&pool).await?;
        Ok(Self { pool })
    }

    async fn migrate(pool: &SqlitePool) -> Result<(), DbError> {
        sqlx::migrate!("./migrations").run(pool).await?;
        Ok(())
    }
}
