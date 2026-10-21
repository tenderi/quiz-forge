//! Members come from Telegram; membership of the configured group is the only
//! permission check (DESIGN.md §3).

use super::{App, AppError, Result};
use crate::db::{parse_ts, ts};
use chrono::{DateTime, Duration, Utc};
use sqlx::SqliteConnection;

pub const MEMBERSHIP_CACHE: Duration = Duration::minutes(10);

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Member {
    pub id: i64,
    pub telegram_id: Option<i64>,
    pub display_name: String,
    pub username: Option<String>,
    pub kind: String,
    pub in_group: bool,
    pub checked_at: Option<String>,
}

impl Member {
    pub fn is_agent(&self) -> bool {
        self.kind == "agent"
    }
}

macro_rules! member_cols {
    () => {
        "id, telegram_id, display_name, username, kind, in_group, checked_at"
    };
}

pub async fn upsert_telegram(
    app: &App,
    telegram_id: i64,
    display_name: &str,
    username: Option<&str>,
    now: DateTime<Utc>,
) -> Result<Member> {
    let name = display_name.trim();
    let name = if name.is_empty() {
        username.unwrap_or("Member")
    } else {
        name
    };
    sqlx::query(
        "INSERT INTO member (telegram_id, display_name, username, created_at) VALUES (?, ?, ?, ?)
         ON CONFLICT (telegram_id) DO UPDATE SET display_name = excluded.display_name, username = excluded.username",
    )
    .bind(telegram_id)
    .bind(name)
    .bind(username)
    .bind(ts(now))
    .execute(&app.db)
    .await?;
    by_telegram(app, telegram_id)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn by_telegram(app: &App, telegram_id: i64) -> Result<Option<Member>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        member_cols!(),
        " FROM member WHERE telegram_id = ?"
    ))
    .bind(telegram_id)
    .fetch_optional(&app.db)
    .await?)
}

pub async fn get(app: &App, id: i64) -> Result<Member> {
    get_conn(&mut *app.db.acquire().await?, id).await
}

pub async fn get_conn(conn: &mut SqliteConnection, id: i64) -> Result<Member> {
    sqlx::query_as(concat!(
        "SELECT ",
        member_cols!(),
        " FROM member WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound)
}

pub async fn list(app: &App) -> Result<Vec<Member>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        member_cols!(),
        " FROM member ORDER BY kind, display_name"
    ))
    .fetch_all(&app.db)
    .await?)
}

/// The review agent's member row, created on first use.
pub async fn agent(app: &App, now: DateTime<Utc>) -> Result<Member> {
    let existing: Option<Member> = sqlx::query_as(concat!(
        "SELECT ",
        member_cols!(),
        " FROM member WHERE kind = 'agent'"
    ))
    .fetch_optional(&app.db)
    .await?;
    if let Some(m) = existing {
        return Ok(m);
    }
    let id = sqlx::query("INSERT INTO member (display_name, kind, in_group, created_at) VALUES ('Review agent', 'agent', 1, ?)")
        .bind(ts(now))
        .execute(&app.db)
        .await?
        .last_insert_rowid();
    get(app, id).await
}

/// Whether the member is (still) in the group. Asks Telegram at most every ten
/// minutes; if Telegram can't be reached, the last known answer stands.
pub async fn ensure_in_group(app: &App, member: &Member, now: DateTime<Utc>) -> Result<bool> {
    if member.is_agent() {
        return Ok(true);
    }
    let Some(tg_id) = member.telegram_id else {
        return Ok(false);
    };
    let fresh = member
        .checked_at
        .as_deref()
        .is_some_and(|c| now - parse_ts(c) < MEMBERSHIP_CACHE);
    if fresh {
        return Ok(member.in_group);
    }
    match app.tg.is_member(app.cfg.group_chat_id, tg_id).await {
        Ok(in_group) => {
            sqlx::query("UPDATE member SET in_group = ?, checked_at = ? WHERE id = ?")
                .bind(in_group)
                .bind(ts(now))
                .bind(member.id)
                .execute(&app.db)
                .await?;
            Ok(in_group)
        }
        Err(e) => {
            tracing::warn!("membership check for member {} failed: {e}", member.id);
            Ok(member.in_group)
        }
    }
}

/// Fails unless the member is in the group.
pub async fn require_in_group(app: &App, member: &Member, now: DateTime<Utc>) -> Result<()> {
    if ensure_in_group(app, member, now).await? {
        Ok(())
    } else {
        Err(AppError::Forbidden(
            "Only members of the quiz group can do that.".into(),
        ))
    }
}
