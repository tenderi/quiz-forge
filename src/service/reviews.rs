//! Recording the AI review pass (DESIGN.md §7): verdicts, auto-applied typo
//! fixes, and proposals for everything else.

use super::members::{self, Member};
use super::outbox::{Outgoing, enqueue};
use super::questions::{self, apply, current_version, file_proposal, question};
use super::{App, OutMessage, Result, esc};
use crate::db::ts;
use crate::domain::model::{Content, Patch};
use crate::domain::proposals::Status;
use crate::domain::{rules, typo_guard};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Ok,
    Doubtful,
    Wrong,
    Unverifiable,
    TimeSensitive,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Ok => "ok",
            Verdict::Doubtful => "doubtful",
            Verdict::Wrong => "wrong",
            Verdict::Unverifiable => "unverifiable",
            Verdict::TimeSensitive => "time_sensitive",
        }
    }
}

/// Spelling fixes, field by field; `None` leaves a field alone.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct TextFixes {
    #[serde(default)]
    pub prompt: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub fun_fact: Option<String>,
    #[serde(default)]
    pub host_notes: Option<String>,
}

/// What a review provider returns for one question.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ReviewResult {
    pub verdict: Verdict,
    pub explanation: String,
    #[serde(default)]
    pub sources: Vec<String>,
    #[serde(default)]
    pub typo_fixes: Option<TextFixes>,
    #[serde(default)]
    pub corrected_answer: Option<String>,
    #[serde(default)]
    pub fun_fact: Option<String>,
    #[serde(default)]
    pub background: Option<String>,
    #[serde(default)]
    pub difficulty: Option<u8>,
    #[serde(default)]
    pub tags: Option<Vec<String>>,
    #[serde(default)]
    pub flags: Vec<String>,
    #[serde(default)]
    pub duplicate_of: Vec<i64>,
}

/// Questions whose current version hasn't been reviewed, oldest change first.
pub async fn pending(app: &App, limit: u32) -> Result<Vec<i64>> {
    Ok(sqlx::query_scalar(
        "SELECT id FROM question WHERE status IN ('voting', 'accepted')
           AND (last_reviewed_version_id IS NULL OR last_reviewed_version_id != current_version_id)
         ORDER BY updated_at, id LIMIT ?",
    )
    .bind(i64::from(limit))
    .fetch_all(&app.db)
    .await?)
}

pub async fn start_run(app: &App, provider: &str, model: &str, now: DateTime<Utc>) -> Result<i64> {
    Ok(
        sqlx::query("INSERT INTO review_run (started_at, provider, model) VALUES (?, ?, ?)")
            .bind(ts(now))
            .bind(provider)
            .bind(model)
            .execute(&app.db)
            .await?
            .last_insert_rowid(),
    )
}

#[allow(clippy::too_many_arguments)]
pub async fn finish_run(
    app: &App,
    run_id: i64,
    questions: u32,
    tokens_in: u64,
    tokens_out: u64,
    cost: f64,
    error: Option<&str>,
    now: DateTime<Utc>,
) -> Result<()> {
    sqlx::query("UPDATE review_run SET finished_at = ?, questions = ?, tokens_in = ?, tokens_out = ?, cost_usd = ?, error = ? WHERE id = ?")
        .bind(ts(now))
        .bind(i64::from(questions))
        .bind(tokens_in as i64)
        .bind(tokens_out as i64)
        .bind(cost)
        .bind(error)
        .bind(run_id)
        .execute(&app.db)
        .await?;
    Ok(())
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct Recorded {
    pub review_id: i64,
    pub auto_applied: Option<i64>,
    pub proposal: Option<i64>,
    pub typo_rejected: Option<String>,
}

fn with_fixes(c: &Content, f: &TextFixes) -> Content {
    let mut n = c.clone();
    if let Some(p) = &f.prompt {
        n.prompt = p.clone();
    }
    if let Some(b) = &f.background {
        n.background = b.clone();
    }
    if let Some(x) = &f.fun_fact {
        n.fun_fact = x.clone();
    }
    if let Some(h) = &f.host_notes {
        n.host_notes = h.clone();
    }
    n
}

/// The non-typo suggestions as one content change on top of `c`.
fn suggestions(c: &Content, r: &ReviewResult) -> Content {
    let mut n = c.clone();
    if let Some(a) = &r.corrected_answer {
        n.answer = a.clone();
    }
    if let Some(f) = r.fun_fact.as_ref().filter(|_| c.fun_fact.is_empty()) {
        n.fun_fact = f.clone();
    }
    if let Some(b) = r.background.as_ref().filter(|_| c.background.is_empty()) {
        n.background = b.clone();
    }
    if let Some(d) = r.difficulty.filter(|d| (1..=5).contains(d)) {
        n.difficulty = d;
    }
    if let Some(t) = &r.tags {
        let mut tags = c.tags.clone();
        tags.extend(t.iter().cloned());
        n.tags = tags;
    }
    n.normalized()
}

/// Stores one review and acts on it. `version_id` is the version the agent saw;
/// if the question moved on meanwhile, only the record is kept and the newer
/// version gets reviewed next run.
pub async fn record(
    app: &App,
    run_id: i64,
    qid: i64,
    version_id: i64,
    r: &ReviewResult,
    now: DateTime<Utc>,
) -> Result<Recorded> {
    let agent = members::agent(app, now).await?;
    let mut tx = app.db.begin().await?;
    let q = question(&mut tx, qid).await?;
    let mut out = Recorded::default();
    let still_current = q.current_version_id == Some(version_id)
        && matches!(q.status(), Status::Voting | Status::Accepted);

    if still_current {
        let cur = current_version(&mut tx, &q).await?;
        let base = cur.content();
        let mut after_typos = base.clone();
        if let Some(fixes) = &r.typo_fixes {
            let fixed = with_fixes(&base, fixes).normalized();
            if fixed != base {
                let guard = typo_guard::check(&base, &fixed);
                let report = rules::check(&fixed);
                match (guard, report.ok()) {
                    (Ok(()), true) => {
                        let vid = apply(
                            &mut tx,
                            app,
                            &q,
                            agent.id,
                            "Typo fixes by the review agent",
                            &fixed,
                            &report,
                            false,
                            now,
                        )
                        .await?;
                        out.auto_applied = Some(vid);
                        after_typos = fixed.clone();
                        let diff = Patch::between(&base, &fixed)
                            .describe(&base)
                            .into_iter()
                            .map(|(f, a, b)| {
                                format!(
                                    "{}: {} → {}",
                                    f.label(),
                                    esc(a.as_str().unwrap_or("")),
                                    esc(b.as_str().unwrap_or(""))
                                )
                            })
                            .collect::<Vec<_>>()
                            .join("\n");
                        let msg = format!(
                            "🔤 The review agent fixed typos in your question #{qid}:\n{diff}\nYou can revert this from the question's history."
                        );
                        enqueue(
                            &mut tx,
                            &Outgoing::Direct {
                                member_id: q.author_id,
                                msg: OutMessage::text(msg),
                            },
                            None,
                            now,
                        )
                        .await?;
                    }
                    (Err(e), _) => out.typo_rejected = Some(e.to_string()),
                    (Ok(()), false) => {
                        out.typo_rejected = Some("the fixed text breaks the house rules".into())
                    }
                }
            }
        }
        // Everything else, plus typo fixes the guard refused, as one proposal.
        let mut proposed = suggestions(&after_typos, r);
        if out.typo_rejected.is_some()
            && let Some(fixes) = &r.typo_fixes
        {
            proposed = with_fixes(&proposed, fixes).normalized();
        }
        let q_now = question(&mut tx, qid).await?;
        let cur_now = current_version(&mut tx, &q_now).await?;
        let patch = Patch::between(&cur_now.content(), &proposed);
        if !patch.is_empty() && rules::check(&proposed).ok() {
            let rationale = format!("Review agent ({}): {}", r.verdict.as_str(), r.explanation);
            out.proposal = Some(
                file_proposal(
                    &mut tx, app, &q_now, &cur_now, &agent, &patch, &rationale, false, now,
                )
                .await?,
            );
        }
    }

    out.review_id = sqlx::query(
        "INSERT INTO review (run_id, question_id, version_id, verdict, explanation, sources, flags, raw, auto_applied_version_id, typo_rejected, created_at)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(run_id)
    .bind(qid)
    .bind(version_id)
    .bind(r.verdict.as_str())
    .bind(&r.explanation)
    .bind(serde_json::to_string(&r.sources)?)
    .bind(serde_json::to_string(&r.flags)?)
    .bind(serde_json::to_string(r)?)
    .bind(out.auto_applied)
    .bind(&out.typo_rejected)
    .bind(ts(now))
    .execute(&mut *tx)
    .await?
    .last_insert_rowid();

    for other in r.duplicate_of.iter().filter(|o| **o != qid) {
        let exists: i64 = sqlx::query_scalar("SELECT count(*) FROM question WHERE id = ?")
            .bind(other)
            .fetch_one(&mut *tx)
            .await?;
        if exists > 0 {
            sqlx::query("INSERT INTO note (question_id, author_id, body, links_question_id, created_at) VALUES (?, ?, ?, ?, ?)")
                .bind(qid)
                .bind(agent.id)
                .bind(format!("The review agent thinks this asks the same thing as #{other}."))
                .bind(other)
                .bind(ts(now))
                .execute(&mut *tx)
                .await?;
        }
    }

    if still_current {
        let flag = (r.verdict != Verdict::Ok || !r.flags.is_empty()).then(|| {
            if r.verdict == Verdict::Ok {
                "flagged".to_string()
            } else {
                r.verdict.as_str().to_string()
            }
        });
        let reviewed = question(&mut tx, qid).await?.current_version_id;
        sqlx::query(
            "UPDATE question SET last_reviewed_version_id = ?, review_flag = ? WHERE id = ?",
        )
        .bind(reviewed)
        .bind(&flag)
        .bind(qid)
        .execute(&mut *tx)
        .await?;
        if matches!(r.verdict, Verdict::Wrong | Verdict::Doubtful) {
            let msg = format!(
                "🔎 The review agent thinks question #{qid} may be {}: {}",
                if r.verdict == Verdict::Wrong {
                    "wrong"
                } else {
                    "doubtful"
                },
                esc(&r.explanation)
            );
            enqueue(
                &mut tx,
                &Outgoing::Direct {
                    member_id: q.author_id,
                    msg: OutMessage::text(msg),
                },
                None,
                now,
            )
            .await?;
        }
    } else {
        sqlx::query("UPDATE question SET last_reviewed_version_id = ? WHERE id = ? AND current_version_id = ?")
            .bind(version_id)
            .bind(qid)
            .bind(version_id)
            .execute(&mut *tx)
            .await?;
    }
    questions::audit(
        &mut tx,
        Some(agent.id),
        "review",
        &format!("question:{qid}"),
        now,
    )
    .await?;
    tx.commit().await?;
    app.outbox_wake.notify_one();
    Ok(out)
}

/// For the agent's prompt: the question and up to `n` likely neighbours.
pub async fn context(app: &App, qid: i64, n: usize) -> Result<(i64, Content, Vec<(i64, String)>)> {
    let mut conn = app.db.acquire().await?;
    let q = question(&mut conn, qid).await?;
    let v = current_version(&mut conn, &q).await?;
    let c = v.content();
    let neighbours: Vec<(i64, String)> = sqlx::query_as(
        "SELECT q.id, v.prompt FROM question q JOIN question_version v ON v.id = q.current_version_id
         WHERE q.id != ? AND v.theme = ? AND q.status IN ('voting', 'accepted') ORDER BY q.id LIMIT ?",
    )
    .bind(qid)
    .bind(&c.theme)
    .bind(n as i64)
    .fetch_all(&mut *conn)
    .await?;
    Ok((v.id, c, neighbours))
}

pub fn agent_member_name(m: &Member) -> &str {
    &m.display_name
}
