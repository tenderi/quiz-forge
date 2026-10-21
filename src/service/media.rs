//! Content-addressed media files under DATA_DIR/media (DESIGN.md §11). Served
//! only through authenticated routes.

use super::{App, AppError, Result};
use crate::db::ts;
use crate::domain::model::MediaKind;
use chrono::{DateTime, Utc};
use std::path::PathBuf;

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Media {
    pub id: i64,
    pub sha256: String,
    pub mime: String,
    pub kind: String,
    pub bytes: i64,
}

impl Media {
    pub fn media_kind(&self) -> MediaKind {
        if self.kind == "audio" {
            MediaKind::Audio
        } else {
            MediaKind::Image
        }
    }
}

/// Recognizes the formats we accept from their first bytes. The client's
/// claimed type is never trusted.
pub fn sniff(b: &[u8]) -> Option<(&'static str, MediaKind)> {
    let starts = |p: &[u8]| b.starts_with(p);
    let at = |off: usize, p: &[u8]| b.len() >= off + p.len() && &b[off..off + p.len()] == p;
    if starts(&[0x89, b'P', b'N', b'G']) {
        Some(("image/png", MediaKind::Image))
    } else if starts(&[0xFF, 0xD8, 0xFF]) {
        Some(("image/jpeg", MediaKind::Image))
    } else if starts(b"GIF8") {
        Some(("image/gif", MediaKind::Image))
    } else if starts(b"RIFF") && at(8, b"WEBP") {
        Some(("image/webp", MediaKind::Image))
    } else if starts(b"RIFF") && at(8, b"WAVE") {
        Some(("audio/wav", MediaKind::Audio))
    } else if starts(b"OggS") {
        Some(("audio/ogg", MediaKind::Audio))
    } else if starts(b"fLaC") {
        Some(("audio/flac", MediaKind::Audio))
    } else if starts(b"ID3") || (b.len() > 1 && b[0] == 0xFF && (b[1] & 0xE0) == 0xE0) {
        Some(("audio/mpeg", MediaKind::Audio))
    } else if at(4, b"ftypM4A") || at(4, b"ftypmp42") || at(4, b"ftypisom") {
        Some(("audio/mp4", MediaKind::Audio))
    } else {
        None
    }
}

pub fn path_for(app: &App, sha: &str) -> PathBuf {
    app.cfg.media_dir().join(&sha[..2]).join(sha)
}

pub async fn store(app: &App, bytes: &[u8], uploaded_by: i64, now: DateTime<Utc>) -> Result<Media> {
    let (mime, kind) = sniff(bytes).ok_or_else(|| {
        AppError::Invalid(
            "Unsupported file type. Use JPEG, PNG, GIF, WebP, MP3, OGG, M4A, FLAC or WAV.".into(),
        )
    })?;
    let cap = match kind {
        MediaKind::Image => app.cfg.media_max_image_bytes,
        MediaKind::Audio => app.cfg.media_max_audio_bytes,
    };
    if bytes.len() as u64 > cap {
        return Err(AppError::Invalid(format!(
            "File is too large (max {} MB).",
            cap / 1024 / 1024
        )));
    }
    let sha = {
        use sha2::{Digest, Sha256};
        hex::encode(Sha256::digest(bytes))
    };
    let path = path_for(app, &sha);
    if !path.exists() {
        let dir = path.parent().expect("media path has a parent");
        tokio::fs::create_dir_all(dir)
            .await
            .map_err(anyhow::Error::from)?;
        let tmp = dir.join(format!(".{sha}.tmp"));
        tokio::fs::write(&tmp, bytes)
            .await
            .map_err(anyhow::Error::from)?;
        tokio::fs::rename(&tmp, &path)
            .await
            .map_err(anyhow::Error::from)?;
    }
    let kind_s = match kind {
        MediaKind::Image => "image",
        MediaKind::Audio => "audio",
    };
    sqlx::query(
        "INSERT INTO media (sha256, mime, kind, bytes, uploaded_by, created_at) VALUES (?, ?, ?, ?, ?, ?)
         ON CONFLICT (sha256) DO NOTHING",
    )
    .bind(&sha)
    .bind(mime)
    .bind(kind_s)
    .bind(bytes.len() as i64)
    .bind(uploaded_by)
    .bind(ts(now))
    .execute(&app.db)
    .await?;
    by_sha(app, &sha).await?.ok_or(AppError::NotFound)
}

pub async fn by_sha(app: &App, sha: &str) -> Result<Option<Media>> {
    Ok(
        sqlx::query_as("SELECT id, sha256, mime, kind, bytes FROM media WHERE sha256 = ?")
            .bind(sha)
            .fetch_optional(&app.db)
            .await?,
    )
}

pub async fn get(app: &App, id: i64) -> Result<Media> {
    sqlx::query_as("SELECT id, sha256, mime, kind, bytes FROM media WHERE id = ?")
        .bind(id)
        .fetch_optional(&app.db)
        .await?
        .ok_or(AppError::NotFound)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sniffs_known_formats() {
        let mut webp = b"RIFF\0\0\0\0WEBPVP8 ".to_vec();
        assert_eq!(sniff(&webp).unwrap().0, "image/webp");
        webp[8..12].copy_from_slice(b"WAVE");
        assert_eq!(sniff(&webp).unwrap(), ("audio/wav", MediaKind::Audio));
        assert_eq!(
            sniff(&[0x89, b'P', b'N', b'G', 0, 0]).unwrap().0,
            "image/png"
        );
        assert_eq!(sniff(&[0xFF, 0xD8, 0xFF, 0xE0]).unwrap().0, "image/jpeg");
        assert_eq!(sniff(b"GIF89a").unwrap().0, "image/gif");
        assert_eq!(sniff(b"OggS\0").unwrap().0, "audio/ogg");
        assert_eq!(sniff(b"fLaC").unwrap().0, "audio/flac");
        assert_eq!(sniff(b"ID3\x04").unwrap().0, "audio/mpeg");
        assert_eq!(sniff(&[0xFF, 0xFB, 0x90]).unwrap().0, "audio/mpeg");
        assert_eq!(sniff(b"\0\0\0\x20ftypM4A ").unwrap().0, "audio/mp4");
        assert_eq!(sniff(b"<html>"), None);
        assert_eq!(sniff(b""), None);
    }
}
