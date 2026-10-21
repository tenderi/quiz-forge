//! SQLite pool, migrations and timestamp helpers.

use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::SqlitePool;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use std::path::Path;
use std::str::FromStr;
use std::time::Duration;

pub static MIGRATOR: sqlx::migrate::Migrator = sqlx::migrate!("./migrations");

/// One connection: every write is serialized, which is all a single group's
/// bank needs, and it keeps read-modify-write transactions free of
/// SQLITE_BUSY upgrades. Nothing holds the connection across network calls.
pub async fn open(path: &Path) -> anyhow::Result<SqlitePool> {
    let opts = SqliteConnectOptions::new()
        .filename(path)
        .create_if_missing(true)
        .journal_mode(SqliteJournalMode::Wal)
        .synchronous(SqliteSynchronous::Normal)
        .foreign_keys(true)
        .busy_timeout(Duration::from_secs(10));
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .connect_with(opts)
        .await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

/// A fresh, migrated in-memory database (tests and tooling).
pub async fn memory() -> anyhow::Result<SqlitePool> {
    let opts = SqliteConnectOptions::from_str("sqlite::memory:")?.foreign_keys(true);
    let pool = SqlitePoolOptions::new()
        .max_connections(1)
        .idle_timeout(None)
        .max_lifetime(None)
        .connect_with(opts)
        .await?;
    MIGRATOR.run(&pool).await?;
    Ok(pool)
}

/// Fixed-width RFC 3339 in UTC, so text order is time order.
pub fn ts(t: DateTime<Utc>) -> String {
    t.to_rfc3339_opts(SecondsFormat::Micros, true)
}

pub fn parse_ts(s: &str) -> DateTime<Utc> {
    DateTime::parse_from_rfc3339(s)
        .map(|t| t.with_timezone(&Utc))
        .unwrap_or_else(|e| panic!("bad timestamp in database '{s}': {e}"))
}

/// Writes a consistent copy of the database to `dest` (VACUUM INTO).
pub async fn backup(pool: &SqlitePool, dest: &Path) -> anyhow::Result<()> {
    if dest.exists() {
        anyhow::bail!("{} already exists", dest.display());
    }
    sqlx::query("VACUUM INTO ?")
        .bind(dest.to_string_lossy().to_string())
        .execute(pool)
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[test]
    fn timestamps_sort_as_text() {
        let a = Utc.with_ymd_and_hms(2026, 1, 2, 3, 4, 5).unwrap();
        let b = a + chrono::Duration::milliseconds(1);
        assert_eq!(ts(a), "2026-01-02T03:04:05.000000Z");
        assert!(ts(a) < ts(b));
        assert_eq!(parse_ts(&ts(b)), b);
    }

    #[tokio::test]
    async fn migrations_apply_and_backup_works() {
        let dir = tempfile::tempdir().unwrap();
        let pool = open(&dir.path().join("q.db")).await.unwrap();
        let n: i64 = sqlx::query_scalar("SELECT count(*) FROM question")
            .fetch_one(&pool)
            .await
            .unwrap();
        assert_eq!(n, 0);
        let dest = dir.path().join("copy.db");
        backup(&pool, &dest).await.unwrap();
        assert!(dest.exists());
        assert!(backup(&pool, &dest).await.is_err(), "refuses to overwrite");
        let mem = memory().await.unwrap();
        let fk: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(&mem)
            .await
            .unwrap();
        assert_eq!(fk, 1);
    }
}
