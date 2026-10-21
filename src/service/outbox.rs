//! Durable outgoing Telegram messages (DESIGN.md §11). Written in the same
//! transaction as the change that causes them; delivered by the scheduler.

use super::{App, OutMessage, Result, TgError, cards};
use crate::db::ts;
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sqlx::SqliteConnection;

pub const MAX_ATTEMPTS: i64 = 10;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Outgoing {
    /// A message to the group.
    Group { msg: OutMessage },
    /// A private message to a member (skipped for members without Telegram).
    Direct { member_id: i64, msg: OutMessage },
    /// Post the vote card of a round; rendered at delivery so it's current.
    VoteCard { question_id: i64, round: i64 },
    /// Re-render a posted vote card in place.
    VoteCardRefresh { question_id: i64, round: i64 },
}

impl Outgoing {
    fn kind(&self) -> &'static str {
        match self {
            Outgoing::Group { .. } => "group",
            Outgoing::Direct { .. } => "direct",
            Outgoing::VoteCard { .. } => "vote_card",
            Outgoing::VoteCardRefresh { .. } => "vote_card_refresh",
        }
    }
}

/// Queues a message. With a `dedupe` key, a second message with the same key is
/// silently dropped.
pub async fn enqueue(
    conn: &mut SqliteConnection,
    item: &Outgoing,
    dedupe: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        "INSERT INTO outbox (kind, payload, dedupe_key, next_attempt_at, created_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (dedupe_key) DO NOTHING",
    )
    .bind(item.kind())
    .bind(serde_json::to_string(item)?)
    .bind(dedupe)
    .bind(ts(now))
    .bind(ts(now))
    .execute(conn)
    .await?;
    Ok(())
}

#[derive(sqlx::FromRow)]
struct Row {
    id: i64,
    payload: String,
    attempts: i64,
}

fn backoff(attempts: i64) -> Duration {
    Duration::seconds((15i64 << attempts.min(8)).min(3600))
}

/// Delivers everything due, oldest first. Returns how many were sent.
pub async fn deliver_due(app: &App, now: DateTime<Utc>) -> Result<usize> {
    let rows: Vec<Row> = sqlx::query_as(
        "SELECT id, payload, attempts FROM outbox
         WHERE sent_at IS NULL AND failed_at IS NULL AND next_attempt_at <= ? ORDER BY id LIMIT 50",
    )
    .bind(ts(now))
    .fetch_all(&app.db)
    .await?;
    let mut sent = 0;
    for row in rows {
        let item: Outgoing = serde_json::from_str(&row.payload)?;
        match deliver(app, &item).await {
            Ok(()) => {
                sqlx::query("UPDATE outbox SET sent_at = ?, attempts = attempts + 1 WHERE id = ?")
                    .bind(ts(now))
                    .bind(row.id)
                    .execute(&app.db)
                    .await?;
                sent += 1;
            }
            Err(e) => {
                let attempts = row.attempts + 1;
                let (next, failed) = match &e {
                    TgError::Permanent(_) => (now, true),
                    TgError::RetryAfter(s) => (
                        now + Duration::seconds(i64::from(*s)),
                        attempts >= MAX_ATTEMPTS,
                    ),
                    TgError::Transient(_) => (now + backoff(attempts), attempts >= MAX_ATTEMPTS),
                };
                tracing::warn!("outbox {} ({}) failed: {e}", row.id, item.kind());
                sqlx::query("UPDATE outbox SET attempts = ?, next_attempt_at = ?, last_error = ?, failed_at = ? WHERE id = ?")
                    .bind(attempts)
                    .bind(ts(next))
                    .bind(e.to_string())
                    .bind(failed.then(|| ts(now)))
                    .bind(row.id)
                    .execute(&app.db)
                    .await?;
            }
        }
    }
    Ok(sent)
}

async fn deliver(app: &App, item: &Outgoing) -> Result<(), TgError> {
    let db = |e: super::AppError| TgError::Transient(format!("database: {e}"));
    match item {
        Outgoing::Group { msg } => app.tg.send(app.cfg.group_chat_id, msg).await.map(|_| ()),
        Outgoing::Direct { member_id, msg } => {
            let tg_id: Option<i64> =
                sqlx::query_scalar("SELECT telegram_id FROM member WHERE id = ?")
                    .bind(member_id)
                    .fetch_optional(&app.db)
                    .await
                    .map_err(|e| db(e.into()))?
                    .flatten();
            match tg_id {
                Some(chat) => app.tg.send(chat, msg).await.map(|_| ()),
                None => Ok(()),
            }
        }
        Outgoing::VoteCard { question_id, round } => {
            let msg = cards::load_vote_card(app, *question_id, *round)
                .await
                .map_err(db)?;
            let id = app.tg.send(app.cfg.group_chat_id, &msg).await?;
            sqlx::query("UPDATE vote_round SET card_chat_id = ?, card_message_id = ? WHERE question_id = ? AND round = ?")
                .bind(app.cfg.group_chat_id)
                .bind(id)
                .bind(question_id)
                .bind(round)
                .execute(&app.db)
                .await
                .map_err(|e| db(e.into()))?;
            Ok(())
        }
        Outgoing::VoteCardRefresh { question_id, round } => {
            let posted: Option<(Option<i64>, Option<i64>)> = sqlx::query_as(
                "SELECT card_chat_id, card_message_id FROM vote_round WHERE question_id = ? AND round = ?",
            )
            .bind(question_id)
            .bind(round)
            .fetch_optional(&app.db)
            .await
            .map_err(|e| db(e.into()))?;
            match posted {
                Some((Some(chat), Some(msg_id))) => {
                    let msg = cards::load_vote_card(app, *question_id, *round)
                        .await
                        .map_err(db)?;
                    app.tg.edit(chat, msg_id, &msg).await
                }
                // Not posted yet: the pending card renders the current state anyway.
                _ => Ok(()),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backoff_grows_and_caps() {
        assert_eq!(backoff(1), Duration::seconds(30));
        assert_eq!(backoff(2), Duration::seconds(60));
        assert_eq!(backoff(20), Duration::seconds(3600));
    }
}
