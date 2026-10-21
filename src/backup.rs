//! `quiz-forge backup <dir>`: a consistent copy of the database (VACUUM INTO)
//! plus any media files the target doesn't have yet. Where the directory goes
//! from there (rclone, rsync, a NAS) is up to the deployer.

use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use std::path::Path;

pub async fn run(
    pool: &SqlitePool,
    media_dir: &Path,
    target: &Path,
    now: DateTime<Utc>,
) -> anyhow::Result<String> {
    std::fs::create_dir_all(target)?;
    let db_file = target.join(format!("quiz-{}.db", now.format("%Y%m%dT%H%M%SZ")));
    crate::db::backup(pool, &db_file).await?;
    let copied = copy_missing(media_dir, &target.join("media"))?;
    Ok(format!(
        "wrote {} and {copied} new media file(s)",
        db_file.display()
    ))
}

/// Copies files from `from` that don't exist in `to` (media is
/// content-addressed, so a file with the same name never changes).
pub fn copy_missing(from: &Path, to: &Path) -> anyhow::Result<usize> {
    let mut n = 0;
    if !from.exists() {
        return Ok(0);
    }
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let dest = to.join(entry.file_name());
        if entry.file_type()?.is_dir() {
            n += copy_missing(&entry.path(), &dest)?;
        } else if !dest.exists() && !entry.file_name().to_string_lossy().starts_with('.') {
            std::fs::create_dir_all(to)?;
            std::fs::copy(entry.path(), &dest)?;
            n += 1;
        }
    }
    Ok(n)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    #[tokio::test]
    async fn backs_up_db_and_new_media_only() {
        let dir = tempfile::tempdir().unwrap();
        let pool = crate::db::open(&dir.path().join("q.db")).await.unwrap();
        let media = dir.path().join("media");
        std::fs::create_dir_all(media.join("ab")).unwrap();
        std::fs::write(media.join("ab").join("abc"), b"x").unwrap();
        std::fs::write(media.join("ab").join(".abc.tmp"), b"partial").unwrap();
        let target = dir.path().join("out");
        let now = Utc.with_ymd_and_hms(2026, 10, 21, 3, 0, 0).unwrap();
        let msg = run(&pool, &media, &target, now).await.unwrap();
        assert!(msg.contains("1 new media"));
        assert!(target.join("quiz-20261021T030000Z.db").exists());
        assert!(target.join("media/ab/abc").exists());
        assert!(!target.join("media/ab/.abc.tmp").exists());
        let later = now + chrono::Duration::hours(1);
        assert!(
            run(&pool, &media, &target, later)
                .await
                .unwrap()
                .contains("0 new media")
        );
        assert_eq!(copy_missing(&dir.path().join("nope"), &target).unwrap(), 0);
    }
}
