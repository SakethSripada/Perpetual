//! SQLite persistence layer for Perpetual (sqlx + runtime queries).
//!
//! Exposes a [`Db`] handle and a set of repository modules. The orchestrator
//! core depends on these functions rather than embedding SQL.

use std::path::Path;

use sha2::{Digest, Sha384};
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
        let migrator = sqlx::migrate!("./migrations");
        reconcile_line_endings(pool, &migrator).await?;
        migrator.run(pool).await?;
        Ok(())
    }
}

/// SQLx checksums migration bytes, including line endings. A database created
/// by a Windows build can therefore reject the same migration embedded by a
/// Unix build (or vice versa). Repair only checksums whose SQL is identical
/// after CRLF/LF normalization; real migration edits still fail validation.
async fn reconcile_line_endings(
    pool: &SqlitePool,
    migrator: &sqlx::migrate::Migrator,
) -> Result<(), sqlx::Error> {
    let table_exists: Option<(String,)> = sqlx::query_as(
        "SELECT name FROM sqlite_master WHERE type = 'table' AND name = '_sqlx_migrations'",
    )
    .fetch_optional(pool)
    .await?;
    if table_exists.is_none() {
        return Ok(());
    }

    let applied: Vec<(i64, Vec<u8>)> =
        sqlx::query_as("SELECT version, checksum FROM _sqlx_migrations")
            .fetch_all(pool)
            .await?;
    for (version, stored) in applied {
        let Some(migration) = migrator.iter().find(|item| item.version == version) else {
            continue;
        };
        if stored == migration.checksum.as_ref() {
            continue;
        }
        let lf = migration.sql.replace("\r\n", "\n").replace('\r', "\n");
        let crlf = lf.replace('\n', "\r\n");
        let matches_line_ending_variant = [lf.as_bytes(), crlf.as_bytes()]
            .iter()
            .any(|sql| Sha384::digest(sql).as_slice() == stored.as_slice());
        if matches_line_ending_variant {
            sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = ?")
                .bind(migration.checksum.as_ref())
                .bind(version)
                .execute(pool)
                .await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod migration_tests {
    use super::*;

    #[tokio::test]
    async fn repairs_only_line_ending_checksum_changes() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap();
        let migrator = sqlx::migrate!("./migrations");
        migrator.run(&pool).await.unwrap();

        let migration = migrator.iter().find(|item| item.version == 21).unwrap();
        let crlf = migration
            .sql
            .replace("\r\n", "\n")
            .replace('\r', "\n")
            .replace('\n', "\r\n");
        let alternate = Sha384::digest(crlf.as_bytes()).to_vec();
        sqlx::query("UPDATE _sqlx_migrations SET checksum = ? WHERE version = 21")
            .bind(alternate)
            .execute(&pool)
            .await
            .unwrap();

        reconcile_line_endings(&pool, &migrator).await.unwrap();
        migrator.run(&pool).await.unwrap();
    }

    #[tokio::test]
    async fn preserves_real_migration_mismatches() {
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .connect_with(SqliteConnectOptions::new().in_memory(true))
            .await
            .unwrap();
        let migrator = sqlx::migrate!("./migrations");
        migrator.run(&pool).await.unwrap();
        sqlx::query("UPDATE _sqlx_migrations SET checksum = X'010203' WHERE version = 21")
            .execute(&pool)
            .await
            .unwrap();

        reconcile_line_endings(&pool, &migrator).await.unwrap();
        assert!(migrator.run(&pool).await.is_err());
    }
}
