//! The service layer: every state change loads what it needs, asks the domain
//! layer what should happen, and writes the result together with its outbox
//! messages in one transaction. Time comes in as `now`; nothing here reads the
//! clock.

pub mod auth;
pub mod cards;
pub mod media;
pub mod members;
pub mod outbox;
pub mod questions;
pub mod quizzes;
pub mod reviews;
pub mod settings;
pub mod telegram;

use crate::config::Config;
use crate::domain::rules::RuleReport;
use chrono::{DateTime, Utc};
use sqlx::SqlitePool;
use std::sync::Arc;
use tokio::sync::Notify;

pub use telegram::{Button, OutMessage, Telegram, TgError};

#[derive(Clone)]
pub struct App {
    pub db: SqlitePool,
    pub cfg: Arc<Config>,
    pub tg: Arc<dyn Telegram>,
    /// Poked after anything is queued, so the outbox doesn't wait for the tick.
    pub outbox_wake: Arc<Notify>,
    /// Wall time for the edges (web, bot, scheduler). Services take `now` as a
    /// parameter instead; tests swap this for a fixed clock.
    pub clock: Arc<dyn Fn() -> DateTime<Utc> + Send + Sync>,
}

impl App {
    pub fn new(db: SqlitePool, cfg: Config, tg: Arc<dyn Telegram>) -> App {
        App {
            db,
            cfg: Arc::new(cfg),
            tg,
            outbox_wake: Arc::new(Notify::new()),
            clock: Arc::new(Utc::now),
        }
    }

    pub fn now(&self) -> DateTime<Utc> {
        (self.clock)()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error("not found")]
    NotFound,
    #[error("{0}")]
    Forbidden(String),
    #[error("{0}")]
    Invalid(String),
    #[error("{0}")]
    Conflict(String),
    #[error("the question breaks the house rules")]
    Rules(RuleReport),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

impl From<serde_json::Error> for AppError {
    fn from(e: serde_json::Error) -> Self {
        AppError::Other(e.into())
    }
}

pub type Result<T, E = AppError> = std::result::Result<T, E>;

/// Minimal HTML escaping for Telegram's HTML parse mode and our own pages.
pub fn esc(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '&' => o.push_str("&amp;"),
            '<' => o.push_str("&lt;"),
            '>' => o.push_str("&gt;"),
            '"' => o.push_str("&quot;"),
            _ => o.push(c),
        }
    }
    o
}

/// Random token, hex. The only randomness in the app outside quiz seeds.
pub fn random_token() -> String {
    let bytes: [u8; 32] = rand::random();
    hex::encode(bytes)
}

pub fn sha256_hex(s: &str) -> String {
    crate::domain::text::stable_hash(&[s])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escaping_and_tokens() {
        assert_eq!(
            esc("<b>\"A&B\"</b>"),
            "&lt;b&gt;&quot;A&amp;B&quot;&lt;/b&gt;"
        );
        let a = random_token();
        assert_eq!(a.len(), 64);
        assert_ne!(a, random_token());
        assert_eq!(sha256_hex("x").len(), 64);
        assert!(AppError::Invalid("bad".into()).to_string().contains("bad"));
    }
}
