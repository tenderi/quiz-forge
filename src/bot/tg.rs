//! teloxide glue: the real [`Telegram`] implementation and the update loop.
//! Kept thin; the logic lives in `bot/mod.rs`.

use super::{Chat, From, on_callback, on_media, on_text};
use crate::domain::model::MediaKind;
use crate::service::telegram::Attachment;
use crate::service::{App, Button, OutMessage, Telegram, TgError, media};
use async_trait::async_trait;
use teloxide::net::Download;
use teloxide::prelude::*;
use teloxide::types::{
    ChatId, InlineKeyboardButton, InlineKeyboardMarkup, InputFile, LinkPreviewOptions, MessageId,
    ParseMode, UserId,
};
use teloxide::{ApiError, RequestError};

pub struct TeloxideTelegram {
    pub bot: Bot,
}

pub fn markup(buttons: &[Vec<Button>]) -> Option<InlineKeyboardMarkup> {
    if buttons.is_empty() {
        return None;
    }
    let rows: Vec<Vec<InlineKeyboardButton>> = buttons
        .iter()
        .map(|row| {
            row.iter()
                .filter_map(|b| match b {
                    Button::Callback { text, data } => {
                        Some(InlineKeyboardButton::callback(text.clone(), data.clone()))
                    }
                    Button::Url { text, url } => url::Url::parse(url)
                        .ok()
                        .map(|u| InlineKeyboardButton::url(text.clone(), u)),
                })
                .collect()
        })
        .collect();
    Some(InlineKeyboardMarkup::new(rows))
}

pub fn map_err(e: RequestError) -> TgError {
    match e {
        RequestError::RetryAfter(s) => TgError::RetryAfter(s.seconds()),
        RequestError::Network(e) => TgError::Transient(e.to_string()),
        RequestError::Io(e) => TgError::Transient(e.to_string()),
        RequestError::MigrateToChatId(id) => TgError::Permanent(format!(
            "the group became a supergroup; set TELEGRAM_GROUP_ID={id}"
        )),
        RequestError::Api(api) => TgError::Permanent(api.to_string()),
        other => TgError::Transient(other.to_string()),
    }
}

fn no_preview() -> LinkPreviewOptions {
    LinkPreviewOptions {
        is_disabled: true,
        url: None,
        prefer_small_media: false,
        prefer_large_media: false,
        show_above_text: false,
    }
}

#[async_trait]
impl Telegram for TeloxideTelegram {
    async fn send(&self, chat_id: i64, msg: &OutMessage) -> Result<i64, TgError> {
        let chat = ChatId(chat_id);
        let kb = markup(&msg.buttons);
        let sent = match &msg.file {
            Some(Attachment { path, audio: true }) => {
                let mut r = self
                    .bot
                    .send_audio(chat, InputFile::file(path.clone()))
                    .caption(msg.html.clone())
                    .parse_mode(ParseMode::Html);
                if let Some(k) = kb {
                    r = r.reply_markup(k);
                }
                r.await
            }
            Some(Attachment { path, audio: false }) => {
                let mut r = self
                    .bot
                    .send_photo(chat, InputFile::file(path.clone()))
                    .caption(msg.html.clone())
                    .parse_mode(ParseMode::Html);
                if let Some(k) = kb {
                    r = r.reply_markup(k);
                }
                r.await
            }
            None => {
                let mut r = self
                    .bot
                    .send_message(chat, msg.html.clone())
                    .parse_mode(ParseMode::Html)
                    .link_preview_options(no_preview());
                if let Some(k) = kb {
                    r = r.reply_markup(k);
                }
                r.await
            }
        };
        sent.map(|m| i64::from(m.id.0)).map_err(map_err)
    }

    async fn edit(&self, chat_id: i64, message_id: i64, msg: &OutMessage) -> Result<(), TgError> {
        let mut r = self
            .bot
            .edit_message_text(
                ChatId(chat_id),
                MessageId(message_id as i32),
                msg.html.clone(),
            )
            .parse_mode(ParseMode::Html)
            .link_preview_options(no_preview());
        // Leaving the markup out removes the buttons, which is what a closed card wants.
        if let Some(k) = markup(&msg.buttons) {
            r = r.reply_markup(k);
        }
        match r.await {
            Ok(_) | Err(RequestError::Api(ApiError::MessageNotModified)) => Ok(()),
            Err(e) => Err(map_err(e)),
        }
    }

    async fn is_member(&self, chat_id: i64, user_id: i64) -> Result<bool, TgError> {
        match self
            .bot
            .get_chat_member(ChatId(chat_id), UserId(user_id as u64))
            .await
        {
            Ok(m) => Ok(m.kind.is_present()),
            Err(RequestError::Api(ApiError::UserNotFound)) => Ok(false),
            Err(e) => Err(map_err(e)),
        }
    }
}

#[derive(Clone)]
struct BotName(String);

/// Runs the bot until the process stops.
pub async fn run(app: App, bot: Bot) -> anyhow::Result<()> {
    let me = bot.get_me().await?;
    let name = BotName(me.user.username.clone().unwrap_or_default());
    tracing::info!("bot @{} started", name.0);
    let handler = dptree::entry()
        .branch(Update::filter_message().endpoint(handle_message))
        .branch(Update::filter_callback_query().endpoint(handle_callback));
    Dispatcher::builder(bot, handler)
        .dependencies(dptree::deps![app, name])
        .default_handler(|_| async {})
        .build()
        .dispatch()
        .await;
    Ok(())
}

fn from_user(u: &teloxide::types::User) -> From {
    From {
        id: u.id.0 as i64,
        name: u.full_name(),
        username: u.username.clone(),
    }
}

async fn send_all(app: &App, sends: super::Sends) {
    for (chat, msg) in sends {
        if let Err(e) = app.tg.send(chat, &msg).await {
            tracing::warn!("reply to {chat} failed: {e}");
        }
    }
}

/// Downloads a photo, audio file or voice note and stores it.
async fn fetch_media(
    app: &App,
    bot: &Bot,
    msg: &Message,
    uploader: i64,
) -> anyhow::Result<Option<(i64, MediaKind)>> {
    let file_id = if let Some(p) = msg.photo().and_then(|ps| ps.last()) {
        p.file.id.clone()
    } else if let Some(a) = msg.audio() {
        a.file.id.clone()
    } else if let Some(v) = msg.voice() {
        v.file.id.clone()
    } else {
        return Ok(None);
    };
    let file = bot.get_file(file_id).await?;
    let mut buf: Vec<u8> = Vec::new();
    bot.download_file(&file.path, &mut buf).await?;
    let m = media::store(app, &buf, uploader, app.now()).await?;
    Ok(Some((m.id, m.media_kind())))
}

async fn handle_message(bot: Bot, msg: Message, app: App, name: BotName) -> ResponseResult<()> {
    let Some(user) = msg.from.as_ref() else {
        return Ok(());
    };
    if user.is_bot {
        return Ok(());
    }
    let from = from_user(user);
    let chat = if msg.chat.is_private() {
        Chat::Private
    } else if msg.chat.id.0 == app.cfg.group_chat_id {
        Chat::Group
    } else {
        Chat::Other
    };
    let now = app.now();
    let result = if let Some(t) = msg.text() {
        on_text(&app, chat, msg.chat.id.0, &from, t, &name.0, now)
            .await
            .map_err(anyhow::Error::from)
    } else if chat == Chat::Private {
        match crate::service::members::upsert_telegram(
            &app,
            from.id,
            &from.name,
            from.username.as_deref(),
            now,
        )
        .await
        {
            Ok(m) => match fetch_media(&app, &bot, &msg, m.id).await {
                Ok(Some((id, kind))) => on_media(&app, &from, id, kind, now)
                    .await
                    .map_err(anyhow::Error::from),
                Ok(None) => Ok(vec![]),
                Err(e) => Ok(vec![(
                    from.id,
                    OutMessage::text(format!(
                        "Couldn't take that file: {}",
                        crate::service::esc(&e.to_string())
                    )),
                )]),
            },
            Err(e) => Err(e.into()),
        }
    } else {
        Ok(vec![])
    };
    match result {
        Ok(sends) => send_all(&app, sends).await,
        Err(e) => {
            tracing::error!("handling a message failed: {e:#}");
            let _ = bot
                .send_message(
                    msg.chat.id,
                    "Something went wrong on my side. Try again in a moment.",
                )
                .await;
        }
    }
    Ok(())
}

async fn handle_callback(bot: Bot, q: CallbackQuery, app: App) -> ResponseResult<()> {
    let from = from_user(&q.from);
    let data = q.data.clone().unwrap_or_default();
    let reply = on_callback(&app, &from, &data, app.now()).await;
    let toast = match reply {
        Ok(r) => {
            send_all(&app, r.sends).await;
            r.toast
        }
        Err(e) => {
            tracing::error!("handling a button failed: {e:#}");
            "Something went wrong on my side.".to_string()
        }
    };
    let mut ans = bot.answer_callback_query(q.id.clone());
    if !toast.is_empty() {
        ans = ans.text(toast);
    }
    let _ = ans.await;
    app.outbox_wake.notify_one();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markup_conversion() {
        assert!(markup(&[]).is_none());
        let k = markup(&[vec![
            Button::Callback {
                text: "a".into(),
                data: "x".into(),
            },
            Button::Url {
                text: "u".into(),
                url: "https://example.org".into(),
            },
            Button::Url {
                text: "bad".into(),
                url: "not a url".into(),
            },
        ]])
        .unwrap();
        assert_eq!(k.inline_keyboard[0].len(), 2);
    }

    #[test]
    fn error_mapping() {
        assert_eq!(
            map_err(RequestError::RetryAfter(
                teloxide::types::Seconds::from_seconds(3)
            )),
            TgError::RetryAfter(3)
        );
        assert!(matches!(
            map_err(RequestError::Api(ApiError::BotBlocked)),
            TgError::Permanent(_)
        ));
        assert!(
            matches!(map_err(RequestError::MigrateToChatId(ChatId(-5))), TgError::Permanent(m) if m.contains("-5"))
        );
        assert!(matches!(
            map_err(RequestError::Io(std::sync::Arc::new(
                std::io::Error::other("x")
            ))),
            TgError::Transient(_)
        ));
    }
}
