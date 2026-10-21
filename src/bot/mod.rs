//! The Telegram bot (DESIGN.md §13). This module holds the logic, in terms of
//! plain inputs and [`OutMessage`]s, so it's testable without Telegram;
//! `tg.rs` is the thin teloxide glue.

pub mod tg;
pub mod wizard;

use crate::db::ts;
use crate::domain::model::MediaKind;
use crate::domain::voting::{Outcome, Vote};
use crate::service::members::{self, Member};
use crate::service::questions::{self, DecideOutcome, SubmitOutcome};
use crate::service::{App, AppError, Button, OutMessage, Result, auth, cards, esc, settings};
use chrono::{DateTime, Utc};
use wizard::{Action, Input, Wizard};

/// Who sent an update.
#[derive(Debug, Clone)]
pub struct From {
    pub id: i64,
    pub name: String,
    pub username: Option<String>,
}

/// Where it came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Chat {
    Private,
    /// The configured group.
    Group,
    /// Some other group: ignored.
    Other,
}

/// Messages to send right away, as (chat id, message). State-change
/// notifications go through the outbox instead.
pub type Sends = Vec<(i64, OutMessage)>;

pub const HELP: &str = "<b>quiz-forge</b> collects quiz questions for this group.\n\n\
/add — submit a question (in a private chat with me)\n\
/mine — your questions\n\
/inbox — change proposals waiting for you\n\
/web — a sign-in link for the web app\n\
/random [theme] — a random accepted question\n\
/stats — the bank in numbers\n\
/language [code] — show or set the group's question language\n\
/cancel — stop the current /add";

fn text(chat: i64, html: impl Into<String>) -> (i64, OutMessage) {
    (chat, OutMessage::text(html))
}

async fn member(app: &App, from: &From, now: DateTime<Utc>) -> Result<Member> {
    members::upsert_telegram(app, from.id, &from.name, from.username.as_deref(), now).await
}

/// Members only. Returns the refusal to send otherwise.
async fn gate(
    app: &App,
    from: &From,
    now: DateTime<Utc>,
) -> Result<std::result::Result<Member, Sends>> {
    let m = member(app, from, now).await?;
    if members::ensure_in_group(app, &m, now).await? {
        Ok(Ok(m))
    } else {
        Ok(Err(vec![text(
            from.id,
            "This bot belongs to one Telegram group, and you're not in it.",
        )]))
    }
}

// ---------------------------------------------------------------- wizard state

async fn load_wizard(app: &App, member_id: i64) -> Result<Option<Wizard>> {
    let raw: Option<String> =
        sqlx::query_scalar("SELECT state FROM wizard_state WHERE member_id = ?")
            .bind(member_id)
            .fetch_optional(&app.db)
            .await?;
    Ok(raw.and_then(|r| serde_json::from_str(&r).ok()))
}

async fn save_wizard(app: &App, member_id: i64, w: &Wizard, now: DateTime<Utc>) -> Result<()> {
    sqlx::query(
        "INSERT INTO wizard_state (member_id, state, updated_at) VALUES (?, ?, ?)
         ON CONFLICT (member_id) DO UPDATE SET state = excluded.state, updated_at = excluded.updated_at",
    )
    .bind(member_id)
    .bind(serde_json::to_string(w)?)
    .bind(ts(now))
    .execute(&app.db)
    .await?;
    Ok(())
}

async fn clear_wizard(app: &App, member_id: i64) -> Result<()> {
    sqlx::query("DELETE FROM wizard_state WHERE member_id = ?")
        .bind(member_id)
        .execute(&app.db)
        .await?;
    Ok(())
}

fn wizard_msg(r: wizard::Reply) -> OutMessage {
    OutMessage::text(r.html).with_buttons(r.buttons)
}

async fn start_wizard(app: &App, m: &Member, now: DateTime<Utc>) -> Result<OutMessage> {
    let themes = questions::themes(app).await?;
    let language = settings::default_language(app).await?;
    let (w, r) = wizard::start(
        &language,
        themes.first().map(String::as_str).unwrap_or("misc"),
    );
    save_wizard(app, m.id, &w, now).await?;
    Ok(wizard_msg(r))
}

async fn wizard_input(app: &App, m: &Member, input: Input, now: DateTime<Utc>) -> Result<Sends> {
    let Some(w) = load_wizard(app, m.id).await? else {
        return Ok(vec![text(
            m.telegram_id.unwrap_or_default(),
            "Send /add to submit a question, or /help.",
        )]);
    };
    let chat = m.telegram_id.unwrap_or_default();
    let themes = questions::themes(app).await?;
    let (w, r, action) = wizard::step(w, input, &themes);
    match action {
        Action::None => {
            save_wizard(app, m.id, &w, now).await?;
            Ok(vec![(chat, wizard_msg(r))])
        }
        Action::Cancel => {
            clear_wizard(app, m.id).await?;
            Ok(vec![(chat, wizard_msg(r))])
        }
        Action::Submit { confirm_duplicates } => {
            let outcome = questions::submit(app, m, w.draft.clone(), confirm_duplicates, now).await;
            let (w2, msg) = match outcome {
                Ok(SubmitOutcome::Submitted(id)) => {
                    clear_wizard(app, m.id).await?;
                    let link = cards::web_link(app, &format!("/questions/{id}"));
                    return Ok(vec![text(
                        chat,
                        format!("✅ Submitted as #{id}. The group can vote on it now.{link}"),
                    )]);
                }
                Ok(SubmitOutcome::Refused(report)) => {
                    let problems = report
                        .errors
                        .iter()
                        .map(|e| format!("⛔ {}", e.message()))
                        .collect::<Vec<_>>()
                        .join("\n");
                    wizard::after_refusal(w, &esc(&problems), false, &themes)
                }
                Ok(SubmitOutcome::Duplicates(ms)) => {
                    let list = ms
                        .iter()
                        .map(|x| {
                            format!("• #{} — {} ({})", x.question_id, x.band.label(), x.status)
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    wizard::after_refusal(
                        w,
                        &format!("Similar questions already exist:\n{list}"),
                        true,
                        &themes,
                    )
                }
                Err(AppError::Invalid(e)) => {
                    wizard::after_refusal(w, &format!("⛔ {}", esc(&e)), false, &themes)
                }
                Err(e) => return Err(e),
            };
            save_wizard(app, m.id, &w2, now).await?;
            Ok(vec![(chat, wizard_msg(msg))])
        }
    }
}

// ---------------------------------------------------------------- messages

/// A text message or command.
pub async fn on_text(
    app: &App,
    chat: Chat,
    chat_id: i64,
    from: &From,
    text_in: &str,
    bot_username: &str,
    now: DateTime<Utc>,
) -> Result<Sends> {
    if chat == Chat::Other {
        return Ok(vec![]);
    }
    let t = text_in.trim();
    if let Some(cmd_line) = t.strip_prefix('/') {
        let (cmd, arg) = cmd_line
            .split_once(char::is_whitespace)
            .map(|(c, a)| (c, a.trim()))
            .unwrap_or((cmd_line, ""));
        let cmd = cmd.split('@').next().unwrap_or("").to_lowercase();
        return command(app, chat, chat_id, from, &cmd, arg, bot_username, now).await;
    }
    if chat == Chat::Group {
        return Ok(vec![]);
    }
    let m = match gate(app, from, now).await? {
        Ok(m) => m,
        Err(refusal) => return Ok(refusal),
    };
    wizard_input(app, &m, Input::Text(t.to_string()), now).await
}

/// A photo, audio file or voice note in a private chat, already stored.
pub async fn on_media(
    app: &App,
    from: &From,
    media_id: i64,
    kind: MediaKind,
    now: DateTime<Utc>,
) -> Result<Sends> {
    let m = match gate(app, from, now).await? {
        Ok(m) => m,
        Err(refusal) => return Ok(refusal),
    };
    wizard_input(app, &m, Input::Media { media_id, kind }, now).await
}

#[allow(clippy::too_many_arguments)]
async fn command(
    app: &App,
    chat: Chat,
    chat_id: i64,
    from: &From,
    cmd: &str,
    arg: &str,
    bot_username: &str,
    now: DateTime<Utc>,
) -> Result<Sends> {
    let dm = from.id;
    let private_only = |what: &str| -> Sends {
        vec![text(
            chat_id,
            format!("Send {what} to me in a private chat: https://t.me/{bot_username}"),
        )]
    };
    match (cmd, chat) {
        ("help", _) => Ok(vec![text(chat_id, HELP)]),
        ("start", Chat::Private) => {
            let m = match gate(app, from, now).await? {
                Ok(m) => m,
                Err(r) => return Ok(r),
            };
            if arg == "add" {
                return Ok(vec![(dm, start_wizard(app, &m, now).await?)]);
            }
            Ok(vec![text(
                dm,
                format!("Hi {}! {HELP}", esc(&m.display_name)),
            )])
        }
        ("language", _) => {
            if arg.is_empty() {
                let l = settings::default_language(app).await?;
                return Ok(vec![text(
                    chat_id,
                    format!(
                        "New questions are in <b>{}</b>. Change it with /language &lt;code&gt;, e.g. /language en.",
                        esc(&l)
                    ),
                )]);
            }
            let m = match gate(app, from, now).await? {
                Ok(m) => m,
                Err(r) => return Ok(r),
            };
            match settings::set_default_language(app, arg, &m, now).await {
                Ok(l) => Ok(vec![text(
                    chat_id,
                    format!(
                        "{} set the group's question language to <b>{}</b>. A single question can still use another one (Language on the review screen).",
                        esc(&m.display_name),
                        esc(&l)
                    ),
                )]),
                Err(AppError::Invalid(e)) => Ok(vec![text(chat_id, esc(&e))]),
                Err(e) => Err(e),
            }
        }
        ("stats", _) => {
            let s = questions::stats(app).await?;
            let status = s
                .by_status
                .iter()
                .map(|(k, n)| format!("{k}: {n}"))
                .collect::<Vec<_>>()
                .join(" · ");
            let themes = s
                .by_theme
                .iter()
                .take(8)
                .map(|(k, n)| format!("{} {n}", esc(k)))
                .collect::<Vec<_>>()
                .join(", ");
            let top = s
                .top_authors
                .iter()
                .map(|(k, n)| format!("{} {n}", esc(k)))
                .collect::<Vec<_>>()
                .join(", ");
            Ok(vec![text(
                chat_id,
                format!("📊 {status}\nThemes: {themes}\nTop authors: {top}"),
            )])
        }
        ("add", Chat::Group) => {
            let m = member(app, from, now).await?;
            let mut out = vec![text(
                chat_id,
                format!(
                    "{}, I've sent you a private message. Nothing there? Start a chat with me first: https://t.me/{bot_username}?start=add",
                    esc(&m.display_name)
                ),
            )];
            out.push((dm, start_wizard(app, &m, now).await?));
            Ok(out)
        }
        ("add", Chat::Private) => {
            let m = match gate(app, from, now).await? {
                Ok(m) => m,
                Err(r) => return Ok(r),
            };
            Ok(vec![(dm, start_wizard(app, &m, now).await?)])
        }
        ("cancel", Chat::Private) => {
            let m = member(app, from, now).await?;
            clear_wizard(app, m.id).await?;
            Ok(vec![text(dm, "Cancelled.")])
        }
        ("mine", Chat::Private) => {
            let m = match gate(app, from, now).await? {
                Ok(m) => m,
                Err(r) => return Ok(r),
            };
            let rows = questions::list(
                app,
                &questions::Filter {
                    author_id: Some(m.id),
                    limit: 20,
                    ..Default::default()
                },
            )
            .await?;
            if rows.is_empty() {
                return Ok(vec![text(dm, "You haven't submitted anything yet. /add")]);
            }
            let lines: Vec<String> = rows
                .iter()
                .map(|s| {
                    let extra = if s.open_proposals > 0 {
                        format!(" · {} proposal(s)", s.open_proposals)
                    } else {
                        String::new()
                    };
                    format!("#{} {} — {}{extra}", s.id, s.status, esc(&s.prompt))
                })
                .collect();
            Ok(vec![text(dm, lines.join("\n"))])
        }
        ("inbox", Chat::Private) => {
            let m = match gate(app, from, now).await? {
                Ok(m) => m,
                Err(r) => return Ok(r),
            };
            let items = questions::inbox(app, &m).await?;
            if items.is_empty() {
                return Ok(vec![text(dm, "No proposals waiting for you.")]);
            }
            let mut out = Vec::new();
            for (p, prompt) in items {
                let fields: Vec<String> = p
                    .patch()
                    .fields()
                    .map(|f| f.label().to_lowercase())
                    .collect();
                let link = cards::web_link(
                    app,
                    &format!("/questions/{}#proposal-{}", p.question_id, p.id),
                );
                let html = format!(
                    "Proposal #{} on #{} (“{}”): {}{link}",
                    p.id,
                    p.question_id,
                    esc(&prompt),
                    fields.join(", ")
                );
                let buttons = vec![vec![
                    Button::Callback {
                        text: "✅ Accept".into(),
                        data: format!("p:{}:a", p.id),
                    },
                    Button::Callback {
                        text: "❌ Decline".into(),
                        data: format!("p:{}:d", p.id),
                    },
                ]];
                out.push((dm, OutMessage::text(html).with_buttons(buttons)));
            }
            Ok(out)
        }
        ("web", Chat::Private) => {
            let m = match gate(app, from, now).await? {
                Ok(m) => m,
                Err(r) => return Ok(r),
            };
            let token = auth::create_login_token(app, &m, now).await?;
            Ok(vec![text(
                dm,
                format!(
                    "Your sign-in link (valid 10 minutes, works once):\n{}",
                    esc(&auth::login_url(app, &token))
                ),
            )])
        }
        ("random", Chat::Private) => {
            let m = match gate(app, from, now).await? {
                Ok(m) => m,
                Err(r) => return Ok(r),
            };
            let pick = u64::from_str_radix(&crate::service::random_token()[..12], 16).unwrap_or(0);
            let theme = (!arg.is_empty()).then_some(arg);
            match questions::random_accepted(app, &m, theme, pick, now).await? {
                Some((id, c)) => Ok(vec![text(dm, cards::question_text(id, &c))]),
                None => Ok(vec![text(dm, "No accepted questions there yet.")]),
            }
        }
        ("mine" | "inbox" | "web" | "random" | "cancel", _) => Ok(private_only(&format!("/{cmd}"))),
        _ => Ok(if chat == Chat::Private {
            vec![text(dm, HELP)]
        } else {
            vec![]
        }),
    }
}

// ---------------------------------------------------------------- buttons

#[derive(Debug, Clone, PartialEq, Default)]
pub struct CallbackReply {
    /// Shown as a toast on the button press.
    pub toast: String,
    pub sends: Sends,
}

fn toast(s: impl Into<String>) -> CallbackReply {
    CallbackReply {
        toast: s.into(),
        sends: vec![],
    }
}

pub async fn on_callback(
    app: &App,
    from: &From,
    data: &str,
    now: DateTime<Utc>,
) -> Result<CallbackReply> {
    let m = match gate(app, from, now).await? {
        Ok(m) => m,
        Err(_) => return Ok(toast("Only members of the quiz group can do that.")),
    };
    let parts: Vec<&str> = data.split(':').collect();
    let res = match parts.as_slice() {
        ["v", qid, round, v] => {
            let (Ok(qid), Ok(round), Some(vote)) =
                (qid.parse::<i64>(), round.parse::<i64>(), Vote::parse(v))
            else {
                return Ok(toast("That button is broken."));
            };
            match questions::vote(app, qid, Some(round), &m, vote, now).await {
                Ok(o) if o.outcome == Some(Outcome::Accepted) => {
                    toast(format!("Accepted! Question #{qid} is in the bank."))
                }
                Ok(o) => toast(format!("Vote recorded: ✅ {} · ❌ {}", o.yes, o.no)),
                Err(e) => toast(e.to_string()),
            }
        }
        ["p", pid, a] => {
            let Ok(pid) = pid.parse::<i64>() else {
                return Ok(toast("That button is broken."));
            };
            match questions::decide(app, pid, &m, *a == "a", now).await {
                Ok(DecideOutcome::Accepted { .. }) => toast("Accepted."),
                Ok(DecideOutcome::Declined) => toast("Declined."),
                Ok(DecideOutcome::Stale(_)) => {
                    toast("Out of date: the question changed since. The proposer was told.")
                }
                Err(AppError::Rules(r)) => toast(
                    r.errors
                        .iter()
                        .map(|e| e.message())
                        .collect::<Vec<_>>()
                        .join(" "),
                ),
                Err(e) => toast(e.to_string()),
            }
        }
        ["w", ..] => {
            let rest = data[2..].to_string();
            if load_wizard(app, m.id).await?.is_none() {
                return Ok(toast("That form has expired. Send /add to start again."));
            }
            CallbackReply {
                toast: String::new(),
                sends: wizard_input(app, &m, Input::Button(rest), now).await?,
            }
        }
        _ => toast("Unknown button."),
    };
    Ok(res)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn help_lists_commands() {
        for c in [
            "/add", "/mine", "/inbox", "/web", "/random", "/stats", "/cancel",
        ] {
            assert!(HELP.contains(c), "{c}");
        }
    }
}
