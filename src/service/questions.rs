//! Questions: submitting, voting, editing, proposals and the read models the
//! bot and web show (DESIGN.md §5).

use super::members::{self, Member};
use super::outbox::{Outgoing, enqueue};
use super::{App, AppError, Button, OutMessage, Result, cards, esc};
use crate::db::{parse_ts, ts};
use crate::domain::dedupe::{self, Candidate, Match};
use crate::domain::model::{Content, Field, Kind, MediaKind, Patch};
use crate::domain::proposals::{self, AuthorEdit, ProposalView, Rebase, Status};
use crate::domain::rules::{self, AnswerClass, Reaction, RuleReport};
use crate::domain::voting::{Outcome, Round, Vote};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use sqlx::SqliteConnection;
use std::collections::BTreeMap;

pub const DUPLICATE_LIMIT: usize = 5;

// ---------------------------------------------------------------- rows

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct QuestionRow {
    pub id: i64,
    pub author_id: i64,
    pub status: String,
    pub vote_round: i64,
    pub current_version_id: Option<i64>,
    pub last_reviewed_version_id: Option<i64>,
    pub review_flag: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

impl QuestionRow {
    pub fn status(&self) -> Status {
        Status::parse(&self.status).expect("valid status in database")
    }

    fn current(&self) -> i64 {
        self.current_version_id.expect("question has a version")
    }
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct VersionRow {
    pub id: i64,
    pub question_id: i64,
    pub n: i64,
    pub created_by: i64,
    pub created_at: String,
    pub reason: String,
    pub content: String,
    pub rule_report: String,
    pub dup_snapshot: String,
}

impl VersionRow {
    pub fn content(&self) -> Content {
        serde_json::from_str(&self.content).expect("valid content json in database")
    }

    pub fn duplicates(&self) -> Vec<Match> {
        serde_json::from_str(&self.dup_snapshot).unwrap_or_default()
    }

    pub fn report(&self) -> Option<RuleReport> {
        serde_json::from_str(&self.rule_report).ok()
    }
}

macro_rules! q_cols {
    () => {
        "id, author_id, status, vote_round, current_version_id, last_reviewed_version_id, review_flag, created_at, updated_at"
    };
}
macro_rules! v_cols {
    () => {
        "id, question_id, n, created_by, created_at, reason, content, rule_report, dup_snapshot"
    };
}

pub async fn question(conn: &mut SqliteConnection, id: i64) -> Result<QuestionRow> {
    sqlx::query_as(concat!("SELECT ", q_cols!(), " FROM question WHERE id = ?"))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn version(conn: &mut SqliteConnection, id: i64) -> Result<VersionRow> {
    sqlx::query_as(concat!(
        "SELECT ",
        v_cols!(),
        " FROM question_version WHERE id = ?"
    ))
    .bind(id)
    .fetch_optional(conn)
    .await?
    .ok_or(AppError::NotFound)
}

pub async fn current_version(conn: &mut SqliteConnection, q: &QuestionRow) -> Result<VersionRow> {
    version(conn, q.current()).await
}

// ---------------------------------------------------------------- themes

pub async fn themes(app: &App) -> Result<Vec<String>> {
    Ok(sqlx::query_scalar("SELECT name FROM theme ORDER BY name")
        .fetch_all(&app.db)
        .await?)
}

pub fn normalize_theme(name: &str) -> String {
    name.split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

pub async fn add_theme(
    app: &App,
    name: &str,
    by: Option<i64>,
    now: DateTime<Utc>,
) -> Result<String> {
    let name = normalize_theme(name);
    if name.is_empty() || name.chars().count() > 40 {
        return Err(AppError::Invalid("A theme name is 1–40 characters.".into()));
    }
    sqlx::query("INSERT INTO theme (name, created_by, created_at) VALUES (?, ?, ?) ON CONFLICT (name) DO NOTHING")
        .bind(&name)
        .bind(by)
        .bind(ts(now))
        .execute(&app.db)
        .await?;
    Ok(name)
}

/// Seeds the theme list from data/themes.toml on first start.
pub async fn seed_themes(app: &App, now: DateTime<Utc>) -> Result<()> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM theme")
        .fetch_one(&app.db)
        .await?;
    if n == 0 {
        #[derive(Deserialize)]
        struct T {
            themes: Vec<String>,
        }
        let t: T = toml::from_str(include_str!("../../data/themes.toml"))
            .expect("data/themes.toml is valid");
        for name in t.themes {
            add_theme(app, &name, None, now).await?;
        }
    }
    Ok(())
}

async fn theme_exists(conn: &mut SqliteConnection, name: &str) -> Result<bool> {
    let n: i64 = sqlx::query_scalar("SELECT count(*) FROM theme WHERE name = ?")
        .bind(name)
        .fetch_one(conn)
        .await?;
    Ok(n > 0)
}

// ---------------------------------------------------------------- checks

async fn candidates(conn: &mut SqliteConnection) -> Result<Vec<Candidate>> {
    let rows: Vec<(i64, String, String)> = sqlx::query_as(
        "SELECT q.id, q.status, v.content FROM question q JOIN question_version v ON v.id = q.current_version_id",
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(id, status, content)| {
            let c: Content = serde_json::from_str(&content).expect("valid content json");
            Candidate {
                question_id: id,
                status,
                language: c.language.clone(),
                answers: c.all_answers().into_iter().map(String::from).collect(),
                prompt: c.prompt,
            }
        })
        .collect())
}

async fn duplicates(
    conn: &mut SqliteConnection,
    c: &Content,
    exclude: Option<i64>,
) -> Result<Vec<Match>> {
    let cands = candidates(conn).await?;
    let answers: Vec<String> = c.all_answers().into_iter().map(String::from).collect();
    Ok(dedupe::find_similar(
        &c.prompt,
        &answers,
        &c.language,
        &cands,
        exclude,
        DUPLICATE_LIMIT,
    ))
}

/// Media references must point at stored files of the right kind.
async fn check_media(conn: &mut SqliteConnection, c: &Content) -> Result<()> {
    for m in &c.media {
        let kind: Option<String> = sqlx::query_scalar("SELECT kind FROM media WHERE id = ?")
            .bind(m.media_id)
            .fetch_optional(&mut *conn)
            .await?;
        let want = match m.kind {
            MediaKind::Image => "image",
            MediaKind::Audio => "audio",
        };
        if kind.as_deref() != Some(want) {
            return Err(AppError::Invalid(format!(
                "Media #{} isn't an uploaded {want} file.",
                m.media_id
            )));
        }
    }
    Ok(())
}

/// Live feedback for a form: house rules and near-duplicates, no side effects.
pub async fn check_draft(
    app: &App,
    content: Content,
    exclude: Option<i64>,
) -> Result<(Content, RuleReport, Vec<Match>)> {
    let c = content.normalized();
    let report = rules::check(&c);
    let mut conn = app.db.acquire().await?;
    let dups = if c.prompt.is_empty() {
        vec![]
    } else {
        duplicates(&mut conn, &c, exclude).await?
    };
    Ok((c, report, dups))
}

// ---------------------------------------------------------------- reactions

/// Queues the group reactions of a report (shame, Boo). Keyed so that the same
/// content never triggers the same reaction twice.
async fn react(
    conn: &mut SqliteConnection,
    who: &Member,
    c: &Content,
    report: &RuleReport,
    key: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    for r in &report.reactions {
        let html = cards::reaction(*r, &who.display_name, &c.prompt);
        let tag = match r {
            Reaction::Shame => "shame",
            Reaction::Boo => "boo",
        };
        let dedupe_key = format!("react:{tag}:{key}");
        enqueue(
            conn,
            &Outgoing::Group {
                msg: OutMessage::text(html),
            },
            Some(&dedupe_key),
            now,
        )
        .await?;
    }
    Ok(())
}

pub(crate) fn content_key(c: &Content) -> String {
    crate::domain::text::stable_hash(&[&c.prompt, &c.answer])
}

// ---------------------------------------------------------------- submit

#[derive(Debug, Clone, PartialEq)]
pub enum SubmitOutcome {
    Refused(RuleReport),
    /// Near-duplicates exist; submit again with `confirm_duplicates` to go ahead.
    Duplicates(Vec<Match>),
    Submitted(i64),
}

pub async fn submit(
    app: &App,
    author: &Member,
    content: Content,
    confirm_duplicates: bool,
    now: DateTime<Utc>,
) -> Result<SubmitOutcome> {
    let c = content.normalized();
    let report = rules::check(&c);
    let mut tx = app.db.begin().await?;
    if !report.ok() {
        react(&mut tx, author, &c, &report, &content_key(&c), now).await?;
        tx.commit().await?;
        app.outbox_wake.notify_one();
        return Ok(SubmitOutcome::Refused(report));
    }
    if !theme_exists(&mut tx, &c.theme).await? {
        return Err(AppError::Invalid(format!(
            "Unknown theme “{}”. Add it first.",
            c.theme
        )));
    }
    check_media(&mut tx, &c).await?;
    let dups = duplicates(&mut tx, &c, None).await?;
    if !dups.is_empty() && !confirm_duplicates {
        return Ok(SubmitOutcome::Duplicates(dups));
    }
    let qid = sqlx::query("INSERT INTO question (author_id, status, vote_round, created_at, updated_at) VALUES (?, 'voting', 1, ?, ?)")
        .bind(author.id)
        .bind(ts(now))
        .bind(ts(now))
        .execute(&mut *tx)
        .await?
        .last_insert_rowid();
    let vid = insert_version(
        &mut tx,
        qid,
        1,
        None,
        author.id,
        "Submitted",
        &c,
        &report,
        &dups,
        now,
    )
    .await?;
    sqlx::query("UPDATE question SET current_version_id = ? WHERE id = ?")
        .bind(vid)
        .bind(qid)
        .execute(&mut *tx)
        .await?;
    open_round(&mut tx, app, qid, 1, &c, now).await?;
    expose(&mut tx, author.id, qid, "author", now).await?;
    react(&mut tx, author, &c, &report, &format!("q{qid}"), now).await?;
    audit(
        &mut tx,
        Some(author.id),
        "submit",
        &format!("question:{qid}"),
        now,
    )
    .await?;
    tx.commit().await?;
    app.outbox_wake.notify_one();
    Ok(SubmitOutcome::Submitted(qid))
}

#[allow(clippy::too_many_arguments)]
async fn insert_version(
    conn: &mut SqliteConnection,
    qid: i64,
    n: i64,
    parent: Option<i64>,
    by: i64,
    reason: &str,
    c: &Content,
    report: &RuleReport,
    dups: &[Match],
    now: DateTime<Utc>,
) -> Result<i64> {
    Ok(sqlx::query(
        "INSERT INTO question_version (question_id, n, parent_id, created_by, created_at, reason, content,
            kind, theme, difficulty, language, prompt, answer, rule_report, dup_snapshot)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(qid)
    .bind(n)
    .bind(parent)
    .bind(by)
    .bind(ts(now))
    .bind(reason)
    .bind(serde_json::to_string(c)?)
    .bind(c.kind.as_str())
    .bind(&c.theme)
    .bind(i64::from(c.difficulty))
    .bind(&c.language)
    .bind(&c.prompt)
    .bind(&c.answer)
    .bind(serde_json::to_string(report)?)
    .bind(serde_json::to_string(dups)?)
    .execute(conn)
    .await?
    .last_insert_rowid())
}

/// Opens voting round `round` and queues its card (media first, so the card
/// lands under the picture).
async fn open_round(
    conn: &mut SqliteConnection,
    app: &App,
    qid: i64,
    round: i64,
    c: &Content,
    now: DateTime<Utc>,
) -> Result<()> {
    let closes = now + app.cfg.vote.window;
    sqlx::query(
        "INSERT INTO vote_round (question_id, round, opened_at, closes_at) VALUES (?, ?, ?, ?)",
    )
    .bind(qid)
    .bind(round)
    .bind(ts(now))
    .bind(ts(closes))
    .execute(&mut *conn)
    .await?;
    for (i, m) in c.media.iter().enumerate() {
        let sha: String = sqlx::query_scalar("SELECT sha256 FROM media WHERE id = ?")
            .bind(m.media_id)
            .fetch_one(&mut *conn)
            .await?;
        let msg = OutMessage {
            html: format!("#{qid} · media {}/{}", i + 1, c.media.len()),
            buttons: vec![],
            file: Some(super::telegram::Attachment {
                path: super::media::path_for(app, &sha),
                audio: m.kind == MediaKind::Audio,
            }),
        };
        enqueue(
            conn,
            &Outgoing::Group { msg },
            Some(&format!("media:q{qid}:r{round}:{i}")),
            now,
        )
        .await?;
    }
    enqueue(
        conn,
        &Outgoing::VoteCard {
            question_id: qid,
            round,
        },
        Some(&format!("card:q{qid}:r{round}")),
        now,
    )
    .await?;
    Ok(())
}

pub async fn expose(
    conn: &mut SqliteConnection,
    member_id: i64,
    qid: i64,
    reason: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    sqlx::query("INSERT INTO exposure (member_id, question_id, reason, at) VALUES (?, ?, ?, ?) ON CONFLICT DO NOTHING")
        .bind(member_id)
        .bind(qid)
        .bind(reason)
        .bind(ts(now))
        .execute(conn)
        .await?;
    Ok(())
}

pub async fn audit(
    conn: &mut SqliteConnection,
    actor: Option<i64>,
    action: &str,
    subject: &str,
    now: DateTime<Utc>,
) -> Result<()> {
    sqlx::query("INSERT INTO audit (at, actor_id, action, subject) VALUES (?, ?, ?, ?)")
        .bind(ts(now))
        .bind(actor)
        .bind(action)
        .bind(subject)
        .execute(conn)
        .await?;
    Ok(())
}

// ---------------------------------------------------------------- voting

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct RoundRow {
    pub question_id: i64,
    pub round: i64,
    pub opened_at: String,
    pub closes_at: String,
    pub closed_at: Option<String>,
    pub outcome: Option<String>,
}

pub async fn round_row(conn: &mut SqliteConnection, qid: i64, round: i64) -> Result<RoundRow> {
    sqlx::query_as("SELECT question_id, round, opened_at, closes_at, closed_at, outcome FROM vote_round WHERE question_id = ? AND round = ?")
        .bind(qid)
        .bind(round)
        .fetch_optional(conn)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn round_votes(
    conn: &mut SqliteConnection,
    qid: i64,
    round: i64,
) -> Result<Vec<(i64, Vote)>> {
    let rows: Vec<(i64, String)> = sqlx::query_as("SELECT member_id, value FROM vote WHERE question_id = ? AND round = ? ORDER BY cast_at, member_id")
        .bind(qid)
        .bind(round)
        .fetch_all(conn)
        .await?;
    Ok(rows
        .into_iter()
        .map(|(m, v)| (m, Vote::parse(&v).expect("valid vote")))
        .collect())
}

async fn load_round(conn: &mut SqliteConnection, q: &QuestionRow) -> Result<Round> {
    let r = round_row(conn, q.id, q.vote_round).await?;
    let votes = round_votes(conn, q.id, q.vote_round).await?;
    Ok(Round {
        author_id: q.author_id,
        opened_at: parse_ts(&r.opened_at),
        closes_at: parse_ts(&r.closes_at),
        votes: votes.into_iter().collect(),
        outcome: r.outcome.as_deref().and_then(Outcome::parse),
    })
}

#[derive(Debug, Clone, PartialEq)]
pub struct VoteOutcome {
    pub outcome: Option<Outcome>,
    pub yes: u32,
    pub no: u32,
}

/// Casts a vote. `round` comes from the card the vote was cast on; a vote on an
/// outdated card is refused rather than counted for the new round.
pub async fn vote(
    app: &App,
    qid: i64,
    round: Option<i64>,
    voter: &Member,
    v: Vote,
    now: DateTime<Utc>,
) -> Result<VoteOutcome> {
    let mut tx = app.db.begin().await?;
    let q = question(&mut tx, qid).await?;
    if q.status() != Status::Voting {
        return Err(AppError::Conflict(format!(
            "Voting on #{qid} is closed ({}).",
            q.status
        )));
    }
    if round.is_some_and(|r| r != q.vote_round) {
        return Err(AppError::Conflict(
            "That card is outdated; the question has changed since. Vote on the newest card."
                .into(),
        ));
    }
    let mut r = load_round(&mut tx, &q).await?;
    let outcome = r
        .cast(voter.id, v, now, &app.cfg.vote)
        .map_err(|e| AppError::Conflict(e.to_string()))?;
    sqlx::query(
        "INSERT INTO vote (question_id, round, member_id, value, cast_at) VALUES (?, ?, ?, ?, ?)
         ON CONFLICT (question_id, round, member_id) DO UPDATE SET value = excluded.value, cast_at = excluded.cast_at",
    )
    .bind(qid)
    .bind(q.vote_round)
    .bind(voter.id)
    .bind(v.as_str())
    .bind(ts(now))
    .execute(&mut *tx)
    .await?;
    expose(&mut tx, voter.id, qid, "vote", now).await?;
    if outcome == Some(Outcome::Accepted) {
        close_round(&mut tx, &q, Outcome::Accepted, now).await?;
    }
    enqueue(
        &mut tx,
        &Outgoing::VoteCardRefresh {
            question_id: qid,
            round: q.vote_round,
        },
        None,
        now,
    )
    .await?;
    tx.commit().await?;
    app.outbox_wake.notify_one();
    let (yes, no) = r.tally();
    Ok(VoteOutcome { outcome, yes, no })
}

async fn close_round(
    conn: &mut SqliteConnection,
    q: &QuestionRow,
    outcome: Outcome,
    now: DateTime<Utc>,
) -> Result<()> {
    sqlx::query(
        "UPDATE vote_round SET closed_at = ?, outcome = ? WHERE question_id = ? AND round = ?",
    )
    .bind(ts(now))
    .bind(outcome.as_str())
    .bind(q.id)
    .bind(q.vote_round)
    .execute(&mut *conn)
    .await?;
    sqlx::query("UPDATE question SET status = ?, updated_at = ? WHERE id = ?")
        .bind(outcome.as_str())
        .bind(ts(now))
        .bind(q.id)
        .execute(&mut *conn)
        .await?;
    let v = current_version(conn, q).await?;
    let html = match outcome {
        Outcome::Accepted => format!(
            "✅ Your question #{} was accepted: “{}”",
            q.id,
            esc(&v.content().prompt)
        ),
        Outcome::Rejected => format!(
            "❌ Your question #{} was rejected: “{}”. You can revise and resubmit it.",
            q.id,
            esc(&v.content().prompt)
        ),
    };
    enqueue(
        conn,
        &Outgoing::Direct {
            member_id: q.author_id,
            msg: OutMessage::text(html),
        },
        None,
        now,
    )
    .await?;
    audit(
        conn,
        None,
        outcome.as_str(),
        &format!("question:{}", q.id),
        now,
    )
    .await?;
    Ok(())
}

/// Rejects every question whose window ran out undecided. Idempotent.
pub async fn expire_votes(app: &App, now: DateTime<Utc>) -> Result<usize> {
    let due: Vec<i64> = sqlx::query_scalar(
        "SELECT r.question_id FROM vote_round r JOIN question q ON q.id = r.question_id AND q.vote_round = r.round
         WHERE r.closed_at IS NULL AND r.closes_at <= ? AND q.status = 'voting' ORDER BY r.question_id",
    )
    .bind(ts(now))
    .fetch_all(&app.db)
    .await?;
    let mut n = 0;
    for qid in due {
        let mut tx = app.db.begin().await?;
        let q = question(&mut tx, qid).await?;
        let mut r = load_round(&mut tx, &q).await?;
        if r.expire(now) == Some(Outcome::Rejected) {
            close_round(&mut tx, &q, Outcome::Rejected, now).await?;
            enqueue(
                &mut tx,
                &Outgoing::VoteCardRefresh {
                    question_id: qid,
                    round: q.vote_round,
                },
                None,
                now,
            )
            .await?;
            n += 1;
        }
        tx.commit().await?;
    }
    if n > 0 {
        app.outbox_wake.notify_one();
    }
    Ok(n)
}

/// Starts the next voting round: the open round (if any) is superseded.
async fn reopen(
    conn: &mut SqliteConnection,
    app: &App,
    q: &QuestionRow,
    c: &Content,
    now: DateTime<Utc>,
) -> Result<i64> {
    sqlx::query(
        "UPDATE vote_round SET closed_at = ?, outcome = 'superseded', superseded_version_id = ?
         WHERE question_id = ? AND round = ? AND closed_at IS NULL",
    )
    .bind(ts(now))
    .bind(q.current_version_id)
    .bind(q.id)
    .bind(q.vote_round)
    .execute(&mut *conn)
    .await?;
    enqueue(
        conn,
        &Outgoing::VoteCardRefresh {
            question_id: q.id,
            round: q.vote_round,
        },
        None,
        now,
    )
    .await?;
    let round = q.vote_round + 1;
    sqlx::query(
        "UPDATE question SET status = 'voting', vote_round = ?, updated_at = ? WHERE id = ?",
    )
    .bind(round)
    .bind(ts(now))
    .bind(q.id)
    .execute(&mut *conn)
    .await?;
    open_round(conn, app, q.id, round, c, now).await?;
    Ok(round)
}

/// Puts a rejected question back to the vote unchanged.
pub async fn resubmit(app: &App, qid: i64, by: &Member, now: DateTime<Utc>) -> Result<i64> {
    let mut tx = app.db.begin().await?;
    let q = question(&mut tx, qid).await?;
    if q.author_id != by.id {
        return Err(AppError::Forbidden(
            "Only the author can resubmit a question.".into(),
        ));
    }
    if q.status() != Status::Rejected {
        return Err(AppError::Conflict(
            "Only rejected questions can be resubmitted.".into(),
        ));
    }
    let c = current_version(&mut tx, &q).await?.content();
    let round = reopen(&mut tx, app, &q, &c, now).await?;
    audit(
        &mut tx,
        Some(by.id),
        "resubmit",
        &format!("question:{qid}"),
        now,
    )
    .await?;
    tx.commit().await?;
    app.outbox_wake.notify_one();
    Ok(round)
}

pub async fn set_retired(
    app: &App,
    qid: i64,
    by: &Member,
    retired: bool,
    now: DateTime<Utc>,
) -> Result<()> {
    let mut tx = app.db.begin().await?;
    let q = question(&mut tx, qid).await?;
    let (from, to) = if retired {
        (Status::Accepted, Status::Retired)
    } else {
        (Status::Retired, Status::Accepted)
    };
    if q.status() != from {
        return Err(AppError::Conflict(format!(
            "Only {} questions can be {}.",
            from.as_str(),
            if retired { "retired" } else { "brought back" }
        )));
    }
    sqlx::query("UPDATE question SET status = ?, updated_at = ? WHERE id = ?")
        .bind(to.as_str())
        .bind(ts(now))
        .bind(qid)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        Some(by.id),
        to.as_str(),
        &format!("question:{qid}"),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------- editing

#[derive(Debug, Clone, PartialEq)]
pub enum EditOutcome {
    NoChange,
    Applied { version_id: i64, reopened: bool },
    Proposed(i64),
}

/// Writes a new current version and starts a new round if asked.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn apply(
    conn: &mut SqliteConnection,
    app: &App,
    q: &QuestionRow,
    by: i64,
    reason: &str,
    c: &Content,
    report: &RuleReport,
    reopen_vote: bool,
    now: DateTime<Utc>,
) -> Result<i64> {
    let cur = current_version(conn, q).await?;
    let dups = duplicates(conn, c, Some(q.id)).await?;
    let vid = insert_version(
        conn,
        q.id,
        cur.n + 1,
        Some(cur.id),
        by,
        reason,
        c,
        report,
        &dups,
        now,
    )
    .await?;
    sqlx::query("UPDATE question SET current_version_id = ?, updated_at = ? WHERE id = ?")
        .bind(vid)
        .bind(ts(now))
        .bind(q.id)
        .execute(&mut *conn)
        .await?;
    if reopen_vote {
        reopen(conn, app, q, c, now).await?;
    } else if q.status() == Status::Voting {
        enqueue(
            conn,
            &Outgoing::VoteCardRefresh {
                question_id: q.id,
                round: q.vote_round,
            },
            None,
            now,
        )
        .await?;
    }
    // A number answer appearing through an edit earns its Boo too.
    let was = cur.report().map(|r| r.answer_class);
    if matches!(
        report.answer_class,
        AnswerClass::Numeric | AnswerClass::Year
    ) && was != Some(report.answer_class)
    {
        let editor = members::get_conn(conn, by).await?;
        react(
            conn,
            &editor,
            c,
            report,
            &format!("q{}:v{}", q.id, cur.n + 1),
            now,
        )
        .await?;
    }
    Ok(vid)
}

/// The one entry point for changing a question: applies directly where the
/// rules allow it, otherwise files a proposal (DESIGN.md §5.4).
pub async fn edit(
    app: &App,
    qid: i64,
    editor: &Member,
    content: Content,
    rationale: &str,
    now: DateTime<Utc>,
) -> Result<EditOutcome> {
    let c = content.normalized();
    let mut tx = app.db.begin().await?;
    let q = question(&mut tx, qid).await?;
    let cur = current_version(&mut tx, &q).await?;
    let patch = Patch::between(&cur.content(), &c);
    if patch.is_empty() {
        return Ok(EditOutcome::NoChange);
    }
    let report = rules::check(&c);
    if !report.ok() {
        react(&mut tx, editor, &c, &report, &content_key(&c), now).await?;
        tx.commit().await?;
        app.outbox_wake.notify_one();
        return Err(AppError::Rules(report));
    }
    if !theme_exists(&mut tx, &c.theme).await? {
        return Err(AppError::Invalid(format!("Unknown theme “{}”.", c.theme)));
    }
    check_media(&mut tx, &c).await?;
    let outcome = if editor.id == q.author_id {
        match proposals::author_edit(q.status(), &patch) {
            AuthorEdit::Apply { reopen_vote } => {
                let reason = if rationale.trim().is_empty() {
                    "Edited by the author"
                } else {
                    rationale.trim()
                };
                let vid = apply(
                    &mut tx,
                    app,
                    &q,
                    editor.id,
                    reason,
                    &c,
                    &report,
                    reopen_vote,
                    now,
                )
                .await?;
                EditOutcome::Applied {
                    version_id: vid,
                    reopened: reopen_vote,
                }
            }
            AuthorEdit::NeedsSecondApproval => EditOutcome::Proposed(
                file_proposal(&mut tx, app, &q, &cur, editor, &patch, rationale, true, now).await?,
            ),
        }
    } else {
        EditOutcome::Proposed(
            file_proposal(
                &mut tx, app, &q, &cur, editor, &patch, rationale, false, now,
            )
            .await?,
        )
    };
    audit(
        &mut tx,
        Some(editor.id),
        "edit",
        &format!("question:{qid}"),
        now,
    )
    .await?;
    tx.commit().await?;
    app.outbox_wake.notify_one();
    Ok(outcome)
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn file_proposal(
    conn: &mut SqliteConnection,
    app: &App,
    q: &QuestionRow,
    base: &VersionRow,
    proposer: &Member,
    patch: &Patch,
    rationale: &str,
    needs_second: bool,
    now: DateTime<Utc>,
) -> Result<i64> {
    let pid = sqlx::query(
        "INSERT INTO proposal (question_id, base_version_id, proposer_id, patch, rationale, material, needs_second_approval, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(q.id)
    .bind(base.id)
    .bind(proposer.id)
    .bind(serde_json::to_string(patch)?)
    .bind(rationale.trim())
    .bind(patch.is_material())
    .bind(needs_second)
    .bind(ts(now))
    .execute(&mut *conn)
    .await?
    .last_insert_rowid();
    expose(conn, proposer.id, q.id, "proposal", now).await?;
    let fields: Vec<&str> = patch.fields().map(Field::label).collect();
    let prompt = esc(&base.content().prompt);
    let buttons = vec![vec![
        Button::Callback {
            text: "✅ Accept".into(),
            data: format!("p:{pid}:a"),
        },
        Button::Callback {
            text: "❌ Decline".into(),
            data: format!("p:{pid}:d"),
        },
    ]];
    let link = cards::web_link(app, &format!("/questions/{}#proposal-{pid}", q.id));
    if needs_second {
        let html = format!(
            "✏️ {} wants to change {} of accepted question #{}: “{prompt}”. Another member needs to approve.{link}",
            esc(&proposer.display_name),
            fields.join(", ").to_lowercase(),
            q.id
        );
        let msg = OutMessage::text(html).with_buttons(buttons);
        enqueue(
            conn,
            &Outgoing::Group { msg },
            Some(&format!("proposal:{pid}")),
            now,
        )
        .await?;
    } else {
        let html = format!(
            "✏️ {} proposes a change to your question #{} (“{prompt}”): {}.{}{link}",
            esc(&proposer.display_name),
            q.id,
            fields.join(", ").to_lowercase(),
            if rationale.trim().is_empty() {
                String::new()
            } else {
                format!("\n<i>{}</i>", esc(rationale.trim()))
            },
        );
        let msg = OutMessage::text(html).with_buttons(buttons);
        enqueue(
            conn,
            &Outgoing::Direct {
                member_id: q.author_id,
                msg,
            },
            Some(&format!("proposal:{pid}")),
            now,
        )
        .await?;
    }
    Ok(pid)
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ProposalRow {
    pub id: i64,
    pub question_id: i64,
    pub base_version_id: i64,
    pub proposer_id: i64,
    pub patch: String,
    pub rationale: String,
    pub material: bool,
    pub needs_second_approval: bool,
    pub status: String,
    pub decided_by: Option<i64>,
    pub decided_at: Option<String>,
    pub applied_version_id: Option<i64>,
    pub created_at: String,
}

impl ProposalRow {
    pub fn patch(&self) -> Patch {
        serde_json::from_str(&self.patch).expect("valid patch json")
    }
}

macro_rules! p_cols {
    () => {
        "id, question_id, base_version_id, proposer_id, patch, rationale, material, needs_second_approval, status, decided_by, decided_at, applied_version_id, created_at"
    };
}

pub async fn proposal(conn: &mut SqliteConnection, id: i64) -> Result<ProposalRow> {
    sqlx::query_as(concat!("SELECT ", p_cols!(), " FROM proposal WHERE id = ?"))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(AppError::NotFound)
}

#[derive(Debug, Clone, PartialEq)]
pub enum DecideOutcome {
    Accepted { version_id: Option<i64> },
    Declined,
    Stale(Vec<Field>),
}

pub async fn decide(
    app: &App,
    pid: i64,
    decider: &Member,
    accept: bool,
    now: DateTime<Utc>,
) -> Result<DecideOutcome> {
    let mut tx = app.db.begin().await?;
    let p = proposal(&mut tx, pid).await?;
    if p.status != "open" {
        return Err(AppError::Conflict(format!(
            "Proposal #{pid} is already {}.",
            p.status
        )));
    }
    let q = question(&mut tx, p.question_id).await?;
    let author = members::get_conn(&mut tx, q.author_id).await?;
    let view = ProposalView {
        proposer_id: p.proposer_id,
        needs_second_approval: p.needs_second_approval,
    };
    proposals::may_decide(view, q.author_id, author.in_group, decider.id)
        .map_err(|e| AppError::Forbidden(e.to_string()))?;
    expose(&mut tx, decider.id, q.id, "decide", now).await?;

    let set_status = |status: &'static str, vid: Option<i64>| {
        sqlx::query("UPDATE proposal SET status = ?, decided_by = ?, decided_at = ?, applied_version_id = ? WHERE id = ?")
            .bind(status)
            .bind(decider.id)
            .bind(ts(now))
            .bind(vid)
            .bind(pid)
    };
    let notify = |text: String| Outgoing::Direct {
        member_id: p.proposer_id,
        msg: OutMessage::text(text),
    };

    if !accept {
        set_status("declined", None).execute(&mut *tx).await?;
        let msg = format!(
            "Your proposal #{pid} on question #{} was declined by {}.",
            q.id,
            esc(&decider.display_name)
        );
        enqueue(&mut tx, &notify(msg), None, now).await?;
        tx.commit().await?;
        app.outbox_wake.notify_one();
        return Ok(DecideOutcome::Declined);
    }

    let base = version(&mut tx, p.base_version_id).await?.content();
    let cur = current_version(&mut tx, &q).await?;
    let patch = p.patch();
    let outcome = match proposals::rebase(&base, &cur.content(), &patch) {
        Rebase::Stale(fields) => {
            set_status("stale", None).execute(&mut *tx).await?;
            let msg = format!(
                "Your proposal #{pid} on question #{} couldn't be applied: {} changed in the meantime. Please redo it on the current version.",
                q.id,
                fields
                    .iter()
                    .map(|f| f.label().to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ")
            );
            enqueue(&mut tx, &notify(msg), None, now).await?;
            DecideOutcome::Stale(fields)
        }
        Rebase::AlreadyApplied => {
            set_status("accepted", None).execute(&mut *tx).await?;
            DecideOutcome::Accepted { version_id: None }
        }
        Rebase::Clean => {
            let c = patch.apply(&cur.content())?.normalized();
            let report = rules::check(&c);
            if !report.ok() {
                return Err(AppError::Rules(report));
            }
            let proposer = members::get_conn(&mut tx, p.proposer_id).await?;
            let reason = format!(
                "Proposal #{pid} by {}, accepted by {}",
                proposer.display_name, decider.display_name
            );
            let reopen_vote = proposals::reopens_vote(q.status(), &patch);
            let vid = apply(
                &mut tx,
                app,
                &q,
                p.proposer_id,
                &reason,
                &c,
                &report,
                reopen_vote,
                now,
            )
            .await?;
            set_status("accepted", Some(vid)).execute(&mut *tx).await?;
            let msg = format!(
                "✅ Your proposal #{pid} on question #{} was accepted by {}.",
                q.id,
                esc(&decider.display_name)
            );
            enqueue(&mut tx, &notify(msg), None, now).await?;
            DecideOutcome::Accepted {
                version_id: Some(vid),
            }
        }
    };
    audit(
        &mut tx,
        Some(decider.id),
        if accept {
            "accept_proposal"
        } else {
            "decline_proposal"
        },
        &format!("proposal:{pid}"),
        now,
    )
    .await?;
    tx.commit().await?;
    app.outbox_wake.notify_one();
    Ok(outcome)
}

pub async fn withdraw(app: &App, pid: i64, by: &Member, now: DateTime<Utc>) -> Result<()> {
    let mut tx = app.db.begin().await?;
    let p = proposal(&mut tx, pid).await?;
    if p.proposer_id != by.id {
        return Err(AppError::Forbidden(
            "Only the proposer can withdraw a proposal.".into(),
        ));
    }
    if p.status != "open" {
        return Err(AppError::Conflict(format!(
            "Proposal #{pid} is already {}.",
            p.status
        )));
    }
    sqlx::query(
        "UPDATE proposal SET status = 'withdrawn', decided_by = ?, decided_at = ? WHERE id = ?",
    )
    .bind(by.id)
    .bind(ts(now))
    .bind(pid)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn add_note(
    app: &App,
    qid: i64,
    by: &Member,
    body: &str,
    links: Option<i64>,
    now: DateTime<Utc>,
) -> Result<i64> {
    let body = body.trim();
    if body.is_empty() {
        return Err(AppError::Invalid("A note can't be empty.".into()));
    }
    let mut conn = app.db.acquire().await?;
    question(&mut conn, qid).await?;
    Ok(sqlx::query("INSERT INTO note (question_id, author_id, body, links_question_id, created_at) VALUES (?, ?, ?, ?, ?)")
        .bind(qid)
        .bind(by.id)
        .bind(body)
        .bind(links)
        .bind(ts(now))
        .execute(&mut *conn)
        .await?
        .last_insert_rowid())
}

// ---------------------------------------------------------------- read models

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub status: Option<String>,
    pub theme: Option<String>,
    pub kind: Option<String>,
    pub difficulty: Option<u8>,
    pub language: Option<String>,
    pub text: Option<String>,
    pub author_id: Option<i64>,
    /// Only questions in voting that this member can still vote on.
    pub needs_vote_by: Option<i64>,
    /// Only questions flagged by the review agent.
    pub flagged: bool,
    pub limit: i64,
    pub offset: i64,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct Summary {
    pub id: i64,
    pub status: String,
    pub author_id: i64,
    pub author_name: String,
    pub vote_round: i64,
    pub review_flag: Option<String>,
    pub kind: String,
    pub theme: String,
    pub difficulty: i64,
    pub language: String,
    pub prompt: String,
    pub answer: String,
    pub updated_at: String,
    pub open_proposals: i64,
    pub closes_at: Option<String>,
}

pub async fn list(app: &App, f: &Filter) -> Result<Vec<Summary>> {
    let mut qb = sqlx::QueryBuilder::<sqlx::Sqlite>::new(
        "SELECT q.id, q.status, q.author_id, m.display_name AS author_name, q.vote_round, q.review_flag,
                v.kind, v.theme, v.difficulty, v.language, v.prompt, v.answer, q.updated_at,
                (SELECT count(*) FROM proposal p WHERE p.question_id = q.id AND p.status = 'open') AS open_proposals,
                (SELECT r.closes_at FROM vote_round r WHERE r.question_id = q.id AND r.round = q.vote_round AND r.closed_at IS NULL) AS closes_at
         FROM question q JOIN question_version v ON v.id = q.current_version_id JOIN member m ON m.id = q.author_id WHERE 1=1",
    );
    if let Some(s) = &f.status {
        qb.push(" AND q.status = ").push_bind(s.clone());
    }
    if let Some(t) = &f.theme {
        qb.push(" AND v.theme = ").push_bind(t.clone());
    }
    if let Some(k) = &f.kind {
        qb.push(" AND v.kind = ").push_bind(k.clone());
    }
    if let Some(d) = f.difficulty {
        qb.push(" AND v.difficulty = ").push_bind(i64::from(d));
    }
    if let Some(l) = &f.language {
        qb.push(" AND v.language = ").push_bind(l.clone());
    }
    if let Some(a) = f.author_id {
        qb.push(" AND q.author_id = ").push_bind(a);
    }
    if let Some(t) = f.text.as_ref().map(|t| t.trim()).filter(|t| !t.is_empty()) {
        let like = format!("%{}%", t.replace(['%', '_'], ""));
        qb.push(" AND (v.prompt LIKE ").push_bind(like.clone());
        qb.push(" OR v.answer LIKE ").push_bind(like.clone());
        qb.push(" OR v.content LIKE ").push_bind(like).push(")");
    }
    if let Some(m) = f.needs_vote_by {
        qb.push(" AND q.status = 'voting' AND q.author_id != ")
            .push_bind(m);
        qb.push(" AND NOT EXISTS (SELECT 1 FROM vote x WHERE x.question_id = q.id AND x.round = q.vote_round AND x.member_id = ")
            .push_bind(m)
            .push(")");
    }
    if f.flagged {
        qb.push(" AND q.review_flag IS NOT NULL");
    }
    qb.push(" ORDER BY q.updated_at DESC, q.id DESC LIMIT ")
        .push_bind(if f.limit > 0 { f.limit } else { 50 })
        .push(" OFFSET ")
        .push_bind(f.offset.max(0));
    Ok(qb.build_query_as().fetch_all(&app.db).await?)
}

#[derive(Debug, Clone)]
pub struct VersionInfo {
    pub row: VersionRow,
    pub content: Content,
    pub by_name: String,
}

#[derive(Debug, Clone)]
pub struct RoundInfo {
    pub row: RoundRow,
    /// (member id, name, vote)
    pub votes: Vec<(i64, String, Vote)>,
}

#[derive(Debug, Clone)]
pub struct ProposalInfo {
    pub row: ProposalRow,
    pub proposer_name: String,
    pub decided_by_name: Option<String>,
    pub base_n: i64,
    pub changes: Vec<(Field, serde_json::Value, serde_json::Value)>,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct ReviewInfo {
    pub id: i64,
    pub version_id: i64,
    pub verdict: String,
    pub explanation: String,
    pub sources: String,
    pub flags: String,
    pub auto_applied_version_id: Option<i64>,
    pub typo_rejected: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct NoteInfo {
    pub id: i64,
    pub author_name: String,
    pub body: String,
    pub links_question_id: Option<i64>,
    pub created_at: String,
}

#[derive(Debug, Clone)]
pub struct Detail {
    pub q: QuestionRow,
    pub author: Member,
    pub current: VersionInfo,
    pub versions: Vec<VersionInfo>,
    pub rounds: Vec<RoundInfo>,
    pub proposals: Vec<ProposalInfo>,
    pub reviews: Vec<ReviewInfo>,
    pub notes: Vec<NoteInfo>,
    pub used_at: Vec<(i64, String, String)>,
}

pub async fn detail(app: &App, qid: i64) -> Result<Detail> {
    let mut conn = app.db.acquire().await?;
    let q = question(&mut conn, qid).await?;
    let author = members::get_conn(&mut conn, q.author_id).await?;
    let names: BTreeMap<i64, String> =
        sqlx::query_as::<_, (i64, String)>("SELECT id, display_name FROM member")
            .fetch_all(&mut *conn)
            .await?
            .into_iter()
            .collect();
    let name = |id: i64| names.get(&id).cloned().unwrap_or_else(|| format!("#{id}"));
    let rows: Vec<VersionRow> = sqlx::query_as(concat!(
        "SELECT ",
        v_cols!(),
        " FROM question_version WHERE question_id = ? ORDER BY n DESC"
    ))
    .bind(qid)
    .fetch_all(&mut *conn)
    .await?;
    let versions: Vec<VersionInfo> = rows
        .into_iter()
        .map(|row| VersionInfo {
            content: row.content(),
            by_name: name(row.created_by),
            row,
        })
        .collect();
    let current = versions
        .iter()
        .find(|v| Some(v.row.id) == q.current_version_id)
        .cloned()
        .ok_or(AppError::NotFound)?;
    let round_rows: Vec<RoundRow> = sqlx::query_as(
        "SELECT question_id, round, opened_at, closes_at, closed_at, outcome FROM vote_round WHERE question_id = ? ORDER BY round DESC",
    )
    .bind(qid)
    .fetch_all(&mut *conn)
    .await?;
    let mut rounds = Vec::new();
    for row in round_rows {
        let votes = round_votes(&mut conn, qid, row.round)
            .await?
            .into_iter()
            .map(|(m, v)| (m, name(m), v))
            .collect();
        rounds.push(RoundInfo { row, votes });
    }
    let prows: Vec<ProposalRow> = sqlx::query_as(concat!(
        "SELECT ",
        p_cols!(),
        " FROM proposal WHERE question_id = ? ORDER BY (status = 'open') DESC, id DESC"
    ))
    .bind(qid)
    .fetch_all(&mut *conn)
    .await?;
    let by_id: BTreeMap<i64, &VersionInfo> = versions.iter().map(|v| (v.row.id, v)).collect();
    let proposals = prows
        .into_iter()
        .map(|row| {
            let base = by_id.get(&row.base_version_id);
            let changes = base
                .map(|b| row.patch().describe(&b.content))
                .unwrap_or_default();
            ProposalInfo {
                proposer_name: name(row.proposer_id),
                decided_by_name: row.decided_by.map(name),
                base_n: base.map(|b| b.row.n).unwrap_or(0),
                changes,
                row,
            }
        })
        .collect();
    let reviews = sqlx::query_as(
        "SELECT id, version_id, verdict, explanation, sources, flags, auto_applied_version_id, typo_rejected, created_at
         FROM review WHERE question_id = ? ORDER BY id DESC",
    )
    .bind(qid)
    .fetch_all(&mut *conn)
    .await?;
    let notes = sqlx::query_as(
        "SELECT n.id, m.display_name AS author_name, n.body, n.links_question_id, n.created_at
         FROM note n JOIN member m ON m.id = n.author_id WHERE n.question_id = ? OR n.links_question_id = ? ORDER BY n.id",
    )
    .bind(qid)
    .bind(qid)
    .fetch_all(&mut *conn)
    .await?;
    let used_at = sqlx::query_as(
        "SELECT e.id, e.title, e.date FROM question_usage u JOIN event e ON e.id = u.event_id WHERE u.question_id = ? ORDER BY e.date",
    )
    .bind(qid)
    .fetch_all(&mut *conn)
    .await?;
    Ok(Detail {
        q,
        author,
        current,
        versions,
        rounds,
        proposals,
        reviews,
        notes,
        used_at,
    })
}

/// Proposals waiting for this member's decision.
pub async fn inbox(app: &App, member: &Member) -> Result<Vec<(ProposalRow, String)>> {
    let rows: Vec<ProposalRow> = sqlx::query_as(concat!(
        "SELECT ",
        p_cols!(),
        " FROM proposal WHERE id IN (
            SELECT p.id FROM proposal p JOIN question q ON q.id = p.question_id JOIN member a ON a.id = q.author_id
            WHERE p.status = 'open' AND p.proposer_id != ?1
              AND (p.needs_second_approval = 1 OR q.author_id = ?1 OR a.in_group = 0))
         ORDER BY id"
    ))
    .bind(member.id)
    .fetch_all(&app.db)
    .await?;
    let mut out = Vec::new();
    let mut conn = app.db.acquire().await?;
    for p in rows {
        let q = question(&mut conn, p.question_id).await?;
        let prompt = current_version(&mut conn, &q).await?.content().prompt;
        out.push((p, prompt));
    }
    Ok(out)
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Stats {
    pub by_status: Vec<(String, i64)>,
    pub by_theme: Vec<(String, i64)>,
    pub top_authors: Vec<(String, i64)>,
}

pub async fn stats(app: &App) -> Result<Stats> {
    Ok(Stats {
        by_status: sqlx::query_as("SELECT status, count(*) FROM question GROUP BY status ORDER BY status").fetch_all(&app.db).await?,
        by_theme: sqlx::query_as(
            "SELECT v.theme, count(*) FROM question q JOIN question_version v ON v.id = q.current_version_id
             WHERE q.status = 'accepted' GROUP BY v.theme ORDER BY count(*) DESC, v.theme",
        )
        .fetch_all(&app.db)
        .await?,
        top_authors: sqlx::query_as(
            "SELECT m.display_name, count(*) FROM question q JOIN member m ON m.id = q.author_id
             GROUP BY q.author_id ORDER BY count(*) DESC, m.display_name LIMIT 5",
        )
        .fetch_all(&app.db)
        .await?,
    })
}

/// A random accepted question for `/random`, chosen by the caller's `pick`
/// (which keeps this function deterministic). Records exposure.
pub async fn random_accepted(
    app: &App,
    member: &Member,
    theme: Option<&str>,
    pick: u64,
    now: DateTime<Utc>,
) -> Result<Option<(i64, Content)>> {
    let ids: Vec<i64> = match theme {
        Some(t) => sqlx::query_scalar(
            "SELECT q.id FROM question q JOIN question_version v ON v.id = q.current_version_id WHERE q.status = 'accepted' AND v.theme = ? ORDER BY q.id",
        )
        .bind(normalize_theme(t))
        .fetch_all(&app.db)
        .await?,
        None => sqlx::query_scalar("SELECT id FROM question WHERE status = 'accepted' ORDER BY id").fetch_all(&app.db).await?,
    };
    if ids.is_empty() {
        return Ok(None);
    }
    let qid = ids[(pick % ids.len() as u64) as usize];
    let mut tx = app.db.begin().await?;
    let q = question(&mut tx, qid).await?;
    let c = current_version(&mut tx, &q).await?.content();
    expose(&mut tx, member.id, qid, "random", now).await?;
    tx.commit().await?;
    Ok(Some((qid, c)))
}

/// Accepted questions ready for quiz building.
pub async fn accepted_with_content(
    conn: &mut SqliteConnection,
) -> Result<Vec<(i64, i64, Content)>> {
    let rows: Vec<(i64, i64, String)> = sqlx::query_as(
        "SELECT q.id, v.id, v.content FROM question q JOIN question_version v ON v.id = q.current_version_id
         WHERE q.status = 'accepted' ORDER BY q.id",
    )
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(q, v, c)| (q, v, serde_json::from_str(&c).expect("valid content json")))
        .collect())
}

pub fn kind_of(s: &str) -> Kind {
    s.parse().unwrap_or(Kind::Text)
}
