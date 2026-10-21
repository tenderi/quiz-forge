//! Group-wide settings members can change (stored in `meta`). Today that is
//! the default language of new questions; `DEFAULT_LANGUAGE` is only its
//! starting value.

use super::members::Member;
use super::questions::audit;
use super::{App, AppError, Result};
use crate::domain::rules::language_ok;
use chrono::{DateTime, Utc};

const LANGUAGE: &str = "default_language";

/// The language new questions get unless their author picks another.
pub async fn default_language(app: &App) -> Result<String> {
    let stored: Option<String> = sqlx::query_scalar("SELECT value FROM meta WHERE key = ?")
        .bind(LANGUAGE)
        .fetch_optional(&app.db)
        .await?;
    Ok(stored.unwrap_or_else(|| app.cfg.default_language.clone()))
}

pub async fn set_default_language(
    app: &App,
    code: &str,
    by: &Member,
    now: DateTime<Utc>,
) -> Result<String> {
    let code = code.trim().to_lowercase();
    if !language_ok(&code) {
        return Err(AppError::Invalid(
            "Use a short language code like fi, en or sv.".into(),
        ));
    }
    let mut tx = app.db.begin().await?;
    sqlx::query("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(LANGUAGE)
        .bind(&code)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        Some(by.id),
        "set_default_language",
        &format!("language:{code}"),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(code)
}
