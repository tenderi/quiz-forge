//! Templates, quizzes, players and events (DESIGN.md §8–9).

use super::members::Member;
use super::questions::{self, accepted_with_content, audit, expose};
use super::{App, AppError, Result, random_token};
use crate::db::ts;
use crate::domain::dedupe::prepare;
use crate::domain::model::Content;
use crate::domain::quizfill::{self, Cand, Filled, SlotPos, Template};
use chrono::{DateTime, Utc};
use sqlx::SqliteConnection;
use std::collections::{BTreeMap, BTreeSet};

// ---------------------------------------------------------------- templates

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct TemplateRow {
    pub id: i64,
    pub name: String,
    pub body: String,
    pub created_by: i64,
    pub created_at: String,
    pub updated_at: String,
}

pub async fn save_template(
    app: &App,
    id: Option<i64>,
    body: &str,
    by: &Member,
    now: DateTime<Utc>,
) -> Result<i64> {
    let t = Template::parse(body).map_err(|e| AppError::Invalid(e.to_string()))?;
    match id {
        Some(id) => {
            let n = sqlx::query("UPDATE template SET name = ?, body = ?, updated_at = ? WHERE id = ?")
                .bind(&t.name)
                .bind(body)
                .bind(ts(now))
                .bind(id)
                .execute(&app.db)
                .await?
                .rows_affected();
            if n == 0 {
                return Err(AppError::NotFound);
            }
            Ok(id)
        }
        None => Ok(sqlx::query("INSERT INTO template (name, body, created_by, created_at, updated_at) VALUES (?, ?, ?, ?, ?)")
            .bind(&t.name)
            .bind(body)
            .bind(by.id)
            .bind(ts(now))
            .bind(ts(now))
            .execute(&app.db)
            .await?
            .last_insert_rowid()),
    }
}

pub async fn templates(app: &App) -> Result<Vec<TemplateRow>> {
    Ok(sqlx::query_as(
        "SELECT id, name, body, created_by, created_at, updated_at FROM template ORDER BY name, id",
    )
    .fetch_all(&app.db)
    .await?)
}

pub async fn template(app: &App, id: i64) -> Result<TemplateRow> {
    sqlx::query_as(
        "SELECT id, name, body, created_by, created_at, updated_at FROM template WHERE id = ?",
    )
    .bind(id)
    .fetch_optional(&app.db)
    .await?
    .ok_or(AppError::NotFound)
}

pub async fn delete_template(app: &App, id: i64) -> Result<()> {
    let mut tx = app.db.begin().await?;
    sqlx::query("UPDATE quiz SET template_id = NULL WHERE template_id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("DELETE FROM template WHERE id = ?")
        .bind(id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

// ---------------------------------------------------------------- players

#[derive(Debug, Clone, PartialEq, sqlx::FromRow)]
pub struct Player {
    pub id: i64,
    pub name: String,
    pub member_id: Option<i64>,
}

pub async fn players(app: &App) -> Result<Vec<Player>> {
    Ok(
        sqlx::query_as("SELECT id, name, member_id FROM player ORDER BY name")
            .fetch_all(&app.db)
            .await?,
    )
}

pub async fn add_player(
    app: &App,
    name: &str,
    member_id: Option<i64>,
    now: DateTime<Utc>,
) -> Result<i64> {
    let name = name.trim();
    if name.is_empty() {
        return Err(AppError::Invalid("A player needs a name.".into()));
    }
    sqlx::query("INSERT INTO player (name, member_id, created_at) VALUES (?, ?, ?)")
        .bind(name)
        .bind(member_id)
        .bind(ts(now))
        .execute(&app.db)
        .await
        .map(|r| r.last_insert_rowid())
        .map_err(|e| match e {
            sqlx::Error::Database(d) if d.is_unique_violation() => {
                AppError::Conflict("That name or member already has a player entry.".into())
            }
            e => e.into(),
        })
}

/// Creates player entries for every group member who doesn't have one yet.
pub async fn import_members_as_players(app: &App, now: DateTime<Utc>) -> Result<u64> {
    Ok(sqlx::query(
        "INSERT OR IGNORE INTO player (name, member_id, created_at)
         SELECT display_name, id, ? FROM member m
         WHERE kind = 'human' AND in_group = 1 AND NOT EXISTS (SELECT 1 FROM player p WHERE p.member_id = m.id)",
    )
    .bind(ts(now))
    .execute(&app.db)
    .await?
    .rows_affected())
}

/// Questions any of these players has seen (DESIGN.md §8).
pub async fn spoiled_for(conn: &mut SqliteConnection, player_ids: &[i64]) -> Result<BTreeSet<i64>> {
    let mut out = BTreeSet::new();
    for pid in player_ids {
        let ids: Vec<i64> = sqlx::query_scalar(
            "SELECT e.question_id FROM exposure e JOIN player p ON p.member_id = e.member_id WHERE p.id = ?1
             UNION SELECT u.question_id FROM question_usage u JOIN event_player ep ON ep.event_id = u.event_id
               JOIN event ev ON ev.id = u.event_id WHERE ep.player_id = ?1 AND ev.held_at IS NOT NULL",
        )
        .bind(pid)
        .fetch_all(&mut *conn)
        .await?;
        out.extend(ids);
    }
    Ok(out)
}

// ---------------------------------------------------------------- quizzes

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct QuizRow {
    pub id: i64,
    pub name: String,
    pub template_id: Option<i64>,
    pub template_body: String,
    pub seed: String,
    pub bank_as_of: String,
    pub bindings: String,
    pub created_by: i64,
    pub created_at: String,
    pub frozen_at: Option<String>,
}

impl QuizRow {
    pub fn template(&self) -> Template {
        Template::parse(&self.template_body).expect("stored template is valid")
    }

    pub fn bindings(&self) -> BTreeMap<String, String> {
        serde_json::from_str(&self.bindings).unwrap_or_default()
    }
}

macro_rules! quiz_cols {
    () => {
        "id, name, template_id, template_body, seed, bank_as_of, bindings, created_by, created_at, frozen_at"
    };
}

pub async fn quiz(conn: &mut SqliteConnection, id: i64) -> Result<QuizRow> {
    sqlx::query_as(concat!("SELECT ", quiz_cols!(), " FROM quiz WHERE id = ?"))
        .bind(id)
        .fetch_optional(conn)
        .await?
        .ok_or(AppError::NotFound)
}

pub async fn quizzes(app: &App) -> Result<Vec<QuizRow>> {
    Ok(sqlx::query_as(concat!(
        "SELECT ",
        quiz_cols!(),
        " FROM quiz ORDER BY id DESC"
    ))
    .fetch_all(&app.db)
    .await?)
}

async fn quiz_players(conn: &mut SqliteConnection, quiz_id: i64) -> Result<Vec<i64>> {
    Ok(
        sqlx::query_scalar(
            "SELECT player_id FROM quiz_player WHERE quiz_id = ? ORDER BY player_id",
        )
        .bind(quiz_id)
        .fetch_all(conn)
        .await?,
    )
}

async fn load_cands(
    conn: &mut SqliteConnection,
    player_ids: &[i64],
) -> Result<(Vec<Cand>, BTreeMap<i64, Content>)> {
    let spoiled = spoiled_for(conn, player_ids).await?;
    let used: BTreeSet<i64> = sqlx::query_scalar(
        "SELECT DISTINCT u.question_id FROM question_usage u JOIN event e ON e.id = u.event_id WHERE e.held_at IS NOT NULL",
    )
    .fetch_all(&mut *conn)
    .await?
    .into_iter()
    .collect();
    let mut cands = Vec::new();
    let mut contents = BTreeMap::new();
    for (qid, _vid, c) in accepted_with_content(conn).await? {
        let answers: Vec<String> = c.all_answers().into_iter().map(String::from).collect();
        cands.push(Cand {
            question_id: qid,
            kind: c.kind,
            theme: c.theme.clone(),
            difficulty: c.difficulty,
            tags: c.tags.clone(),
            language: c.language.clone(),
            used: used.contains(&qid),
            spoiled: spoiled.contains(&qid),
            dedupe: prepare(&c.prompt, &answers, &c.language),
        });
        contents.insert(qid, c);
    }
    Ok((cands, contents))
}

async fn write_slots(conn: &mut SqliteConnection, quiz_id: i64, filled: &Filled) -> Result<()> {
    sqlx::query("DELETE FROM quiz_slot WHERE quiz_id = ?")
        .bind(quiz_id)
        .execute(&mut *conn)
        .await?;
    for (ri, round) in filled.rounds.iter().enumerate() {
        for (si, s) in round.iter().enumerate() {
            sqlx::query(
                "INSERT INTO quiz_slot (quiz_id, round_ix, slot_ix, question_id, locked, empty_reason, warnings) VALUES (?, ?, ?, ?, ?, ?, ?)",
            )
            .bind(quiz_id)
            .bind(ri as i64)
            .bind(si as i64)
            .bind(s.question_id)
            .bind(s.locked)
            .bind(&s.empty_reason)
            .bind(serde_json::to_string(&s.warnings)?)
            .execute(&mut *conn)
            .await?;
        }
    }
    sqlx::query("UPDATE quiz SET bindings = ? WHERE id = ?")
        .bind(serde_json::to_string(&filled.bindings)?)
        .bind(quiz_id)
        .execute(&mut *conn)
        .await?;
    Ok(())
}

pub fn new_seed() -> String {
    random_token()[..10].to_string()
}

pub async fn create_quiz(
    app: &App,
    template_id: i64,
    name: &str,
    seed: Option<&str>,
    player_ids: &[i64],
    by: &Member,
    now: DateTime<Utc>,
) -> Result<i64> {
    let t = template(app, template_id).await?;
    let tpl = Template::parse(&t.body).map_err(|e| AppError::Invalid(e.to_string()))?;
    let seed = seed
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or_else(new_seed);
    let name = if name.trim().is_empty() {
        tpl.name.clone()
    } else {
        name.trim().to_string()
    };
    let mut tx = app.db.begin().await?;
    let qid = sqlx::query(
        "INSERT INTO quiz (name, template_id, template_body, seed, bank_as_of, created_by, created_at) VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(&name)
    .bind(template_id)
    .bind(&t.body)
    .bind(&seed)
    .bind(ts(now))
    .bind(by.id)
    .bind(ts(now))
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();
    for p in player_ids {
        sqlx::query("INSERT OR IGNORE INTO quiz_player (quiz_id, player_id) VALUES (?, ?)")
            .bind(qid)
            .bind(p)
            .execute(&mut *tx)
            .await?;
    }
    let (cands, _) = load_cands(&mut tx, player_ids).await?;
    let filled = quizfill::fill(&tpl, &cands, &seed, &BTreeMap::new());
    write_slots(&mut tx, qid, &filled).await?;
    audit(
        &mut tx,
        Some(by.id),
        "create_quiz",
        &format!("quiz:{qid}"),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(qid)
}

async fn locks(conn: &mut SqliteConnection, quiz_id: i64) -> Result<BTreeMap<SlotPos, i64>> {
    let rows: Vec<(i64, i64, i64)> = sqlx::query_as(
        "SELECT round_ix, slot_ix, question_id FROM quiz_slot WHERE quiz_id = ? AND locked = 1 AND question_id IS NOT NULL",
    )
    .bind(quiz_id)
    .fetch_all(conn)
    .await?;
    Ok(rows
        .into_iter()
        .map(|(r, s, q)| ((r as usize, s as usize), q))
        .collect())
}

fn ensure_open(q: &QuizRow) -> Result<()> {
    if q.frozen_at.is_some() {
        return Err(AppError::Conflict(
            "This quiz is frozen. Unfreeze it to change questions.".into(),
        ));
    }
    Ok(())
}

/// Fills every unlocked slot again, with a new seed if given.
pub async fn refill(app: &App, quiz_id: i64, seed: Option<&str>, now: DateTime<Utc>) -> Result<()> {
    let mut tx = app.db.begin().await?;
    let q = quiz(&mut tx, quiz_id).await?;
    ensure_open(&q)?;
    let seed = seed
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(String::from)
        .unwrap_or(q.seed.clone());
    let players = quiz_players(&mut tx, quiz_id).await?;
    let (cands, _) = load_cands(&mut tx, &players).await?;
    let l = locks(&mut tx, quiz_id).await?;
    let filled = quizfill::fill(&q.template(), &cands, &seed, &l);
    write_slots(&mut tx, quiz_id, &filled).await?;
    sqlx::query("UPDATE quiz SET seed = ?, bank_as_of = ? WHERE id = ?")
        .bind(&seed)
        .bind(ts(now))
        .bind(quiz_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn set_players(app: &App, quiz_id: i64, player_ids: &[i64]) -> Result<()> {
    let mut tx = app.db.begin().await?;
    quiz(&mut tx, quiz_id).await?;
    sqlx::query("DELETE FROM quiz_player WHERE quiz_id = ?")
        .bind(quiz_id)
        .execute(&mut *tx)
        .await?;
    for p in player_ids {
        sqlx::query("INSERT OR IGNORE INTO quiz_player (quiz_id, player_id) VALUES (?, ?)")
            .bind(quiz_id)
            .bind(p)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(())
}

/// Puts a question in a slot by hand; the slot becomes locked.
pub async fn set_slot(app: &App, quiz_id: i64, pos: SlotPos, question_id: i64) -> Result<()> {
    let mut tx = app.db.begin().await?;
    let q = quiz(&mut tx, quiz_id).await?;
    ensure_open(&q)?;
    let status: Option<String> = sqlx::query_scalar("SELECT status FROM question WHERE id = ?")
        .bind(question_id)
        .fetch_optional(&mut *tx)
        .await?;
    if status.as_deref() != Some("accepted") {
        return Err(AppError::Invalid(format!(
            "Question #{question_id} isn't an accepted question."
        )));
    }
    let elsewhere: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM quiz_slot WHERE quiz_id = ? AND question_id = ? AND NOT (round_ix = ? AND slot_ix = ?)",
    )
    .bind(quiz_id)
    .bind(question_id)
    .bind(pos.0 as i64)
    .bind(pos.1 as i64)
    .fetch_one(&mut *tx)
    .await?;
    if elsewhere > 0 {
        return Err(AppError::Conflict(format!(
            "Question #{question_id} is already in this quiz."
        )));
    }
    let n = sqlx::query(
        "UPDATE quiz_slot SET question_id = ?, locked = 1, empty_reason = NULL, warnings = '[]' WHERE quiz_id = ? AND round_ix = ? AND slot_ix = ?",
    )
    .bind(question_id)
    .bind(quiz_id)
    .bind(pos.0 as i64)
    .bind(pos.1 as i64)
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if n == 0 {
        return Err(AppError::NotFound);
    }
    tx.commit().await?;
    Ok(())
}

pub async fn set_locked(app: &App, quiz_id: i64, pos: SlotPos, locked: bool) -> Result<()> {
    let mut tx = app.db.begin().await?;
    let q = quiz(&mut tx, quiz_id).await?;
    ensure_open(&q)?;
    let n = sqlx::query("UPDATE quiz_slot SET locked = ? WHERE quiz_id = ? AND round_ix = ? AND slot_ix = ? AND question_id IS NOT NULL")
        .bind(locked)
        .bind(quiz_id)
        .bind(pos.0 as i64)
        .bind(pos.1 as i64)
        .execute(&mut *tx)
        .await?
        .rows_affected();
    if n == 0 {
        return Err(AppError::Invalid(
            "Only a filled slot can be locked.".into(),
        ));
    }
    tx.commit().await?;
    Ok(())
}

pub async fn clear_slot(app: &App, quiz_id: i64, pos: SlotPos) -> Result<()> {
    let mut tx = app.db.begin().await?;
    let q = quiz(&mut tx, quiz_id).await?;
    ensure_open(&q)?;
    sqlx::query(
        "UPDATE quiz_slot SET question_id = NULL, locked = 0, empty_reason = 'Cleared by hand', warnings = '[]'
         WHERE quiz_id = ? AND round_ix = ? AND slot_ix = ?",
    )
    .bind(quiz_id)
    .bind(pos.0 as i64)
    .bind(pos.1 as i64)
    .execute(&mut *tx)
    .await?;
    tx.commit().await?;
    Ok(())
}

/// Pins every slot to its question's current version (DESIGN.md §9.3).
pub async fn freeze(app: &App, quiz_id: i64, now: DateTime<Utc>) -> Result<()> {
    let mut tx = app.db.begin().await?;
    let q = quiz(&mut tx, quiz_id).await?;
    ensure_open(&q)?;
    let empty: i64 = sqlx::query_scalar(
        "SELECT count(*) FROM quiz_slot WHERE quiz_id = ? AND question_id IS NULL",
    )
    .bind(quiz_id)
    .fetch_one(&mut *tx)
    .await?;
    if empty > 0 {
        return Err(AppError::Conflict(format!(
            "{empty} slot(s) are still empty. Fill or swap them before freezing."
        )));
    }
    sqlx::query(
        "UPDATE quiz_slot SET version_id = (SELECT current_version_id FROM question WHERE question.id = quiz_slot.question_id)
         WHERE quiz_id = ?",
    )
    .bind(quiz_id)
    .execute(&mut *tx)
    .await?;
    sqlx::query("UPDATE quiz SET frozen_at = ? WHERE id = ?")
        .bind(ts(now))
        .bind(quiz_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

pub async fn unfreeze(app: &App, quiz_id: i64) -> Result<()> {
    let mut tx = app.db.begin().await?;
    quiz(&mut tx, quiz_id).await?;
    let held: i64 =
        sqlx::query_scalar("SELECT count(*) FROM event WHERE quiz_id = ? AND held_at IS NOT NULL")
            .bind(quiz_id)
            .fetch_one(&mut *tx)
            .await?;
    if held > 0 {
        return Err(AppError::Conflict(
            "This quiz has been played at an event; it stays frozen.".into(),
        ));
    }
    sqlx::query("UPDATE quiz_slot SET version_id = NULL WHERE quiz_id = ?")
        .bind(quiz_id)
        .execute(&mut *tx)
        .await?;
    sqlx::query("UPDATE quiz SET frozen_at = NULL WHERE id = ?")
        .bind(quiz_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

/// Moves a frozen slot to its question's newest version.
pub async fn take_newer(app: &App, quiz_id: i64, pos: SlotPos) -> Result<()> {
    sqlx::query(
        "UPDATE quiz_slot SET version_id = (SELECT current_version_id FROM question WHERE question.id = quiz_slot.question_id)
         WHERE quiz_id = ? AND round_ix = ? AND slot_ix = ? AND version_id IS NOT NULL",
    )
    .bind(quiz_id)
    .bind(pos.0 as i64)
    .bind(pos.1 as i64)
    .execute(&app.db)
    .await?;
    Ok(())
}

pub async fn delete_quiz(app: &App, quiz_id: i64) -> Result<()> {
    let mut tx = app.db.begin().await?;
    let events: i64 = sqlx::query_scalar("SELECT count(*) FROM event WHERE quiz_id = ?")
        .bind(quiz_id)
        .fetch_one(&mut *tx)
        .await?;
    if events > 0 {
        return Err(AppError::Conflict(
            "This quiz has events; it can't be deleted.".into(),
        ));
    }
    sqlx::query("DELETE FROM quiz WHERE id = ?")
        .bind(quiz_id)
        .execute(&mut *tx)
        .await?;
    tx.commit().await?;
    Ok(())
}

#[derive(Debug, Clone)]
pub struct SlotView {
    pub pos: SlotPos,
    pub question_id: Option<i64>,
    pub version_id: Option<i64>,
    pub locked: bool,
    pub empty_reason: Option<String>,
    pub warnings: Vec<String>,
    /// The pinned version when frozen, else the current one.
    pub content: Option<Content>,
    pub newer_available: bool,
    pub status: Option<String>,
}

#[derive(Debug, Clone)]
pub struct RoundView {
    pub title: String,
    pub slots: Vec<SlotView>,
}

#[derive(Debug, Clone)]
pub struct QuizView {
    pub quiz: QuizRow,
    pub template: Template,
    pub rounds: Vec<RoundView>,
    pub player_ids: Vec<i64>,
}

impl QuizView {
    pub fn question_ids(&self) -> BTreeSet<i64> {
        self.rounds
            .iter()
            .flat_map(|r| r.slots.iter().filter_map(|s| s.question_id))
            .collect()
    }
}

pub async fn view(app: &App, quiz_id: i64) -> Result<QuizView> {
    let mut conn = app.db.acquire().await?;
    let q = quiz(&mut conn, quiz_id).await?;
    let template = q.template();
    #[derive(sqlx::FromRow)]
    struct S {
        round_ix: i64,
        slot_ix: i64,
        question_id: Option<i64>,
        version_id: Option<i64>,
        locked: bool,
        empty_reason: Option<String>,
        warnings: String,
        current_version_id: Option<i64>,
        status: Option<String>,
    }
    let rows: Vec<S> = sqlx::query_as(
        "SELECT s.round_ix, s.slot_ix, s.question_id, s.version_id, s.locked, s.empty_reason, s.warnings,
                q.current_version_id, q.status
         FROM quiz_slot s LEFT JOIN question q ON q.id = s.question_id WHERE s.quiz_id = ? ORDER BY s.round_ix, s.slot_ix",
    )
    .bind(quiz_id)
    .fetch_all(&mut *conn)
    .await?;
    let mut rounds: Vec<RoundView> = template
        .rounds
        .iter()
        .map(|r| RoundView {
            title: r.title.clone(),
            slots: vec![],
        })
        .collect();
    for s in rows {
        let shown = s.version_id.or(s.current_version_id);
        let content = match shown {
            Some(v) => Some(questions::version(&mut conn, v).await?.content()),
            None => None,
        };
        if let Some(r) = rounds.get_mut(s.round_ix as usize) {
            r.slots.push(SlotView {
                pos: (s.round_ix as usize, s.slot_ix as usize),
                question_id: s.question_id,
                version_id: s.version_id,
                locked: s.locked,
                empty_reason: s.empty_reason,
                warnings: serde_json::from_str(&s.warnings).unwrap_or_default(),
                content,
                newer_available: s.version_id.is_some() && s.version_id != s.current_version_id,
                status: s.status,
            });
        }
    }
    let player_ids = quiz_players(&mut conn, quiz_id).await?;
    Ok(QuizView {
        quiz: q,
        template,
        rounds,
        player_ids,
    })
}

/// Alternatives for one slot, best first.
pub async fn swap_options(
    app: &App,
    quiz_id: i64,
    pos: SlotPos,
    limit: usize,
) -> Result<Vec<(i64, Content)>> {
    let v = view(app, quiz_id).await?;
    let mut conn = app.db.acquire().await?;
    let (cands, contents) = load_cands(&mut conn, &v.player_ids).await?;
    let mut taken = v.question_ids();
    if let Some(cur) = v
        .rounds
        .get(pos.0)
        .and_then(|r| r.slots.get(pos.1))
        .and_then(|s| s.question_id)
    {
        taken.insert(cur);
    }
    // A slot bound to a theme variable keeps that theme.
    let bound = v
        .template
        .expanded()
        .get(pos.0)
        .and_then(|r| r.get(pos.1))
        .and_then(|s| s.theme.as_deref())
        .and_then(|t| t.strip_prefix('$'))
        .and_then(|var| {
            v.quiz
                .bindings()
                .get(&format!("r{}.${var}", pos.0 + 1))
                .cloned()
        });
    let ids = quizfill::swap_candidates(
        &v.template,
        &cands,
        &v.quiz.seed,
        pos,
        bound.as_deref(),
        &taken,
        limit,
    );
    Ok(ids
        .into_iter()
        .filter_map(|id| contents.get(&id).cloned().map(|c| (id, c)))
        .collect())
}

// ---------------------------------------------------------------- events

#[derive(Debug, Clone, sqlx::FromRow)]
pub struct EventRow {
    pub id: i64,
    pub quiz_id: i64,
    pub date: String,
    pub title: String,
    pub held_at: Option<String>,
    pub created_at: String,
}

pub async fn create_event(
    app: &App,
    quiz_id: i64,
    date: &str,
    title: &str,
    player_ids: &[i64],
    now: DateTime<Utc>,
) -> Result<i64> {
    let date = date.trim();
    if chrono::NaiveDate::parse_from_str(date, "%Y-%m-%d").is_err() {
        return Err(AppError::Invalid("Date must be YYYY-MM-DD.".into()));
    }
    let mut tx = app.db.begin().await?;
    let q = quiz(&mut tx, quiz_id).await?;
    if q.frozen_at.is_none() {
        return Err(AppError::Conflict(
            "Freeze the quiz before scheduling an event with it.".into(),
        ));
    }
    let title = if title.trim().is_empty() {
        q.name.clone()
    } else {
        title.trim().to_string()
    };
    let id =
        sqlx::query("INSERT INTO event (quiz_id, date, title, created_at) VALUES (?, ?, ?, ?)")
            .bind(quiz_id)
            .bind(date)
            .bind(&title)
            .bind(ts(now))
            .execute(&mut *tx)
            .await?
            .last_insert_rowid();
    for p in player_ids {
        sqlx::query("INSERT OR IGNORE INTO event_player (event_id, player_id) VALUES (?, ?)")
            .bind(id)
            .bind(p)
            .execute(&mut *tx)
            .await?;
    }
    tx.commit().await?;
    Ok(id)
}

pub async fn events(app: &App) -> Result<Vec<EventRow>> {
    Ok(sqlx::query_as("SELECT id, quiz_id, date, title, held_at, created_at FROM event ORDER BY date DESC, id DESC")
        .fetch_all(&app.db)
        .await?)
}

pub async fn event_players(app: &App, event_id: i64) -> Result<Vec<Player>> {
    Ok(sqlx::query_as(
        "SELECT p.id, p.name, p.member_id FROM player p JOIN event_player ep ON ep.player_id = p.id WHERE ep.event_id = ? ORDER BY p.name",
    )
    .bind(event_id)
    .fetch_all(&app.db)
    .await?)
}

/// Records that the event happened: its questions count as used, and every
/// attendee who is a member has now heard them.
pub async fn mark_held(app: &App, event_id: i64, by: &Member, now: DateTime<Utc>) -> Result<()> {
    let mut tx = app.db.begin().await?;
    let e: EventRow = sqlx::query_as(
        "SELECT id, quiz_id, date, title, held_at, created_at FROM event WHERE id = ?",
    )
    .bind(event_id)
    .fetch_optional(&mut *tx)
    .await?
    .ok_or(AppError::NotFound)?;
    if e.held_at.is_some() {
        return Err(AppError::Conflict(
            "This event is already marked as held.".into(),
        ));
    }
    let slots: Vec<(i64, i64)> = sqlx::query_as(
        "SELECT question_id, version_id FROM quiz_slot WHERE quiz_id = ? AND question_id IS NOT NULL AND version_id IS NOT NULL",
    )
    .bind(e.quiz_id)
    .fetch_all(&mut *tx)
    .await?;
    let members: Vec<i64> = sqlx::query_scalar(
        "SELECT p.member_id FROM event_player ep JOIN player p ON p.id = ep.player_id WHERE ep.event_id = ? AND p.member_id IS NOT NULL",
    )
    .bind(event_id)
    .fetch_all(&mut *tx)
    .await?;
    for (qid, vid) in &slots {
        sqlx::query("INSERT OR IGNORE INTO question_usage (question_id, version_id, event_id) VALUES (?, ?, ?)")
            .bind(qid)
            .bind(vid)
            .bind(event_id)
            .execute(&mut *tx)
            .await?;
        for m in &members {
            expose(&mut tx, *m, *qid, "event", now).await?;
        }
    }
    sqlx::query("UPDATE event SET held_at = ? WHERE id = ?")
        .bind(ts(now))
        .bind(event_id)
        .execute(&mut *tx)
        .await?;
    audit(
        &mut tx,
        Some(by.id),
        "event_held",
        &format!("event:{event_id}"),
        now,
    )
    .await?;
    tx.commit().await?;
    Ok(())
}
