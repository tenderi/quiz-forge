//! Web login by one-time link from the bot, then a server-side session
//! (DESIGN.md §11). Only hashes of tokens are stored.

use super::members::{self, Member};
use super::{App, AppError, Result, random_token, sha256_hex};
use crate::db::{parse_ts, ts};
use chrono::{DateTime, Duration, Utc};

pub const LOGIN_TOKEN_TTL: Duration = Duration::minutes(10);
pub const SESSION_TTL: Duration = Duration::days(30);
pub const MEMBERSHIP_RECHECK: Duration = Duration::hours(24);

/// A fresh login link token for `member`. Returns the raw token.
pub async fn create_login_token(app: &App, member: &Member, now: DateTime<Utc>) -> Result<String> {
    let token = random_token();
    sqlx::query("INSERT INTO login_token (token_hash, member_id, expires_at) VALUES (?, ?, ?)")
        .bind(sha256_hex(&token))
        .bind(member.id)
        .bind(ts(now + LOGIN_TOKEN_TTL))
        .execute(&app.db)
        .await?;
    Ok(token)
}

pub fn login_url(app: &App, token: &str) -> String {
    app.cfg.url(&format!("/login/{token}"))
}

#[derive(Debug, Clone, PartialEq)]
pub struct NewSession {
    pub raw_id: String,
    pub member_id: i64,
}

/// Spends a login token and opens a session.
pub async fn redeem(app: &App, token: &str, now: DateTime<Utc>) -> Result<NewSession> {
    let mut tx = app.db.begin().await?;
    let row: Option<(i64, String, Option<String>)> = sqlx::query_as(
        "SELECT member_id, expires_at, used_at FROM login_token WHERE token_hash = ?",
    )
    .bind(sha256_hex(token))
    .fetch_optional(&mut *tx)
    .await?;
    let Some((member_id, expires, used)) = row else {
        return Err(AppError::Forbidden(
            "That login link isn't valid. Send /web to the bot for a new one.".into(),
        ));
    };
    if used.is_some() || parse_ts(&expires) <= now {
        return Err(AppError::Forbidden(
            "That login link has expired or was used already. Send /web to the bot for a new one."
                .into(),
        ));
    }
    sqlx::query("UPDATE login_token SET used_at = ? WHERE token_hash = ?")
        .bind(ts(now))
        .bind(sha256_hex(token))
        .execute(&mut *tx)
        .await?;
    let raw_id = random_token();
    sqlx::query(
        "INSERT INTO session (id_hash, member_id, csrf, created_at, last_seen_at, expires_at, checked_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(sha256_hex(&raw_id))
    .bind(member_id)
    .bind(random_token())
    .bind(ts(now))
    .bind(ts(now))
    .bind(ts(now + SESSION_TTL))
    .bind(ts(now))
    .execute(&mut *tx)
    .await?;
    sqlx::query("DELETE FROM login_token WHERE expires_at < ?")
        .bind(ts(now))
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(NewSession { raw_id, member_id })
}

#[derive(Debug, Clone, PartialEq)]
pub struct Session {
    pub member: Member,
    pub csrf: String,
}

/// Resolves a session cookie. Re-checks group membership once a day and ends
/// the session of someone who left.
pub async fn session(app: &App, raw_id: &str, now: DateTime<Utc>) -> Result<Option<Session>> {
    let hash = sha256_hex(raw_id);
    let row: Option<(i64, String, String, String)> = sqlx::query_as(
        "SELECT member_id, csrf, expires_at, checked_at FROM session WHERE id_hash = ?",
    )
    .bind(&hash)
    .fetch_optional(&app.db)
    .await?;
    let Some((member_id, csrf, expires, checked)) = row else {
        return Ok(None);
    };
    if parse_ts(&expires) <= now {
        logout(app, raw_id).await?;
        return Ok(None);
    }
    let member = members::get(app, member_id).await?;
    let recheck = now - parse_ts(&checked) >= MEMBERSHIP_RECHECK;
    if recheck {
        if !members::ensure_in_group(app, &member, now).await? {
            logout(app, raw_id).await?;
            return Ok(None);
        }
        sqlx::query("UPDATE session SET checked_at = ? WHERE id_hash = ?")
            .bind(ts(now))
            .bind(&hash)
            .execute(&app.db)
            .await?;
    }
    sqlx::query("UPDATE session SET last_seen_at = ? WHERE id_hash = ?")
        .bind(ts(now))
        .bind(&hash)
        .execute(&app.db)
        .await?;
    Ok(Some(Session { member, csrf }))
}

pub async fn logout(app: &App, raw_id: &str) -> Result<()> {
    sqlx::query("DELETE FROM session WHERE id_hash = ?")
        .bind(sha256_hex(raw_id))
        .execute(&app.db)
        .await?;
    Ok(())
}
