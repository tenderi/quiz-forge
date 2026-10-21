//! The narrow slice of Telegram the app uses, behind a trait so tests (and an
//! instance run without a bot token) use a fake.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum Button {
    Callback { text: String, data: String },
    Url { text: String, url: String },
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct OutMessage {
    /// Telegram HTML.
    pub html: String,
    #[serde(default)]
    pub buttons: Vec<Vec<Button>>,
    /// A local file to send as a photo or audio, with `html` as its caption.
    #[serde(default)]
    pub file: Option<Attachment>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Attachment {
    pub path: PathBuf,
    pub audio: bool,
}

impl OutMessage {
    pub fn text(html: impl Into<String>) -> OutMessage {
        OutMessage {
            html: html.into(),
            ..Default::default()
        }
    }

    pub fn with_buttons(mut self, buttons: Vec<Vec<Button>>) -> OutMessage {
        self.buttons = buttons;
        self
    }
}

#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum TgError {
    /// Won't succeed on retry: bot blocked, chat gone, bad request.
    #[error("permanent: {0}")]
    Permanent(String),
    #[error("transient: {0}")]
    Transient(String),
    #[error("rate limited for {0}s")]
    RetryAfter(u32),
}

#[async_trait]
pub trait Telegram: Send + Sync {
    /// Sends a message and returns its message id.
    async fn send(&self, chat_id: i64, msg: &OutMessage) -> Result<i64, TgError>;
    async fn edit(&self, chat_id: i64, message_id: i64, msg: &OutMessage) -> Result<(), TgError>;
    async fn is_member(&self, chat_id: i64, user_id: i64) -> Result<bool, TgError>;
}

/// Records everything; membership answers from a fixed list. Also what runs when
/// no bot token is configured, so the web app works on its own.
#[derive(Default)]
pub struct FakeTelegram {
    pub sent: Mutex<Vec<(i64, OutMessage)>>,
    pub edited: Mutex<Vec<(i64, i64, OutMessage)>>,
    pub non_members: Mutex<Vec<i64>>,
    pub fail_next: Mutex<Vec<TgError>>,
}

impl FakeTelegram {
    pub fn sent_texts(&self) -> Vec<(i64, String)> {
        self.sent
            .lock()
            .unwrap()
            .iter()
            .map(|(c, m)| (*c, m.html.clone()))
            .collect()
    }
}

#[async_trait]
impl Telegram for FakeTelegram {
    async fn send(&self, chat_id: i64, msg: &OutMessage) -> Result<i64, TgError> {
        if let Some(e) = self.fail_next.lock().unwrap().pop() {
            return Err(e);
        }
        let mut s = self.sent.lock().unwrap();
        s.push((chat_id, msg.clone()));
        Ok(s.len() as i64)
    }

    async fn edit(&self, chat_id: i64, message_id: i64, msg: &OutMessage) -> Result<(), TgError> {
        if let Some(e) = self.fail_next.lock().unwrap().pop() {
            return Err(e);
        }
        self.edited
            .lock()
            .unwrap()
            .push((chat_id, message_id, msg.clone()));
        Ok(())
    }

    async fn is_member(&self, _chat_id: i64, user_id: i64) -> Result<bool, TgError> {
        Ok(!self.non_members.lock().unwrap().contains(&user_id))
    }
}
