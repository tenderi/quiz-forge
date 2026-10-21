//! The optional AI review pass (DESIGN.md §7). A provider reviews one
//! question at a time; `run_once` walks the unreviewed questions within the
//! configured count and cost caps and records what comes back.

pub mod anthropic;

use crate::db::{parse_ts, ts};
use crate::domain::model::{Content, MediaKind};
use crate::service::reviews::{self, ReviewResult, Verdict};
use crate::service::{App, Result, media};
use async_trait::async_trait;
use base64::Engine;
use chrono::{DateTime, Duration, Utc};

#[derive(Debug, Clone)]
pub struct ReviewInput {
    pub question_id: i64,
    pub version_id: i64,
    pub content: Content,
    /// Other questions in the same theme, for semantic duplicate checks.
    pub neighbours: Vec<(i64, String)>,
    /// (mime, base64) of attached images, only when AGENT_SEND_MEDIA is on.
    pub images: Vec<(String, String)>,
}

#[derive(Debug, Clone)]
pub struct ProviderOutput {
    pub result: ReviewResult,
    pub usage: anthropic::Usage,
    /// The model that answered (a fallback model, if the first one declined).
    pub model: String,
}

#[async_trait]
pub trait ReviewProvider: Send + Sync {
    fn name(&self) -> &str;
    fn model(&self) -> &str;
    async fn review(&self, input: &ReviewInput) -> anyhow::Result<ProviderOutput>;
}

/// The record kept when the model declines: nothing to apply, but the
/// question isn't retried every night.
pub fn declined(why: &str) -> ReviewResult {
    ReviewResult {
        verdict: Verdict::Unverifiable,
        explanation: format!("Not reviewed: {why}."),
        sources: vec![],
        typo_fixes: None,
        corrected_answer: None,
        fun_fact: None,
        background: None,
        difficulty: None,
        tags: None,
        flags: vec!["not_reviewed".into()],
        duplicate_of: vec![],
    }
}

pub const NEIGHBOURS: usize = 40;

async fn input_for(app: &App, qid: i64) -> Result<ReviewInput> {
    let (version_id, content, neighbours) = reviews::context(app, qid, NEIGHBOURS).await?;
    let mut images = Vec::new();
    if app.cfg.agent.send_media {
        for m in content.media.iter().filter(|m| m.kind == MediaKind::Image) {
            let row = media::get(app, m.media_id).await?;
            if let Ok(bytes) = tokio::fs::read(media::path_for(app, &row.sha256)).await {
                images.push((
                    row.mime,
                    base64::engine::general_purpose::STANDARD.encode(bytes),
                ));
            }
        }
    }
    Ok(ReviewInput {
        question_id: qid,
        version_id,
        content,
        neighbours,
        images,
    })
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct RunSummary {
    pub run_id: i64,
    pub reviewed: u32,
    pub failed: u32,
    pub cost_usd: f64,
    pub stopped_on_budget: bool,
}

/// One review pass. The clock is only read for timestamps.
pub async fn run_once(app: &App, provider: &dyn ReviewProvider) -> Result<RunSummary> {
    let cfg = &app.cfg.agent;
    let pending = reviews::pending(app, cfg.max_questions_per_run).await?;
    let run_id = reviews::start_run(app, provider.name(), provider.model(), app.now()).await?;
    let mut s = RunSummary {
        run_id,
        ..Default::default()
    };
    let (mut tin, mut tout) = (0u64, 0u64);
    let mut errors = Vec::new();
    for qid in pending {
        if s.cost_usd >= cfg.max_cost_usd_per_run {
            s.stopped_on_budget = true;
            break;
        }
        let input = input_for(app, qid).await?;
        match provider.review(&input).await {
            Ok(out) => {
                tin += out.usage.tokens_in;
                tout += out.usage.tokens_out;
                s.cost_usd += out.usage.cost_usd;
                reviews::record(app, run_id, qid, input.version_id, &out.result, app.now()).await?;
                s.reviewed += 1;
            }
            Err(e) => {
                tracing::warn!("review of question {qid} failed: {e:#}");
                errors.push(format!("#{qid}: {e}"));
                s.failed += 1;
            }
        }
    }
    let err = (!errors.is_empty()).then(|| errors.join("; "));
    reviews::finish_run(
        app,
        run_id,
        s.reviewed,
        tin,
        tout,
        s.cost_usd,
        err.as_deref(),
        app.now(),
    )
    .await?;
    Ok(s)
}

const LAST_RUN: &str = "agent_last_run";

/// Whether a run is due, by the configured interval since the last one.
pub async fn due(app: &App, now: DateTime<Utc>) -> Result<bool> {
    let last: Option<String> = sqlx::query_scalar("SELECT value FROM meta WHERE key = ?")
        .bind(LAST_RUN)
        .fetch_optional(&app.db)
        .await?;
    Ok(match last {
        Some(t) => now - parse_ts(&t) >= Duration::hours(i64::from(app.cfg.agent.interval_hours)),
        None => true,
    })
}

pub async fn mark_ran(app: &App, now: DateTime<Utc>) -> Result<()> {
    sqlx::query("INSERT INTO meta (key, value) VALUES (?, ?) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(LAST_RUN)
        .bind(ts(now))
        .execute(&app.db)
        .await?;
    Ok(())
}
