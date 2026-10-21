//! Shared test harness: an in-memory database, a fake Telegram and a clock
//! that only moves when told to.
#![allow(dead_code)]

use chrono::{DateTime, Duration, TimeZone, Utc};
use quiz_forge::config::Config;
use quiz_forge::domain::model::{Content, Kind, MediaKind, MediaRef};
use quiz_forge::service::members::{self, Member};
use quiz_forge::service::questions::{self, SubmitOutcome};
use quiz_forge::service::telegram::FakeTelegram;
use quiz_forge::service::{App, outbox};
use std::sync::Arc;
use tempfile::TempDir;

pub struct H {
    pub app: App,
    pub tg: Arc<FakeTelegram>,
    pub dir: TempDir,
    pub now: DateTime<Utc>,
}

pub const GROUP: i64 = -100;

pub async fn h() -> H {
    h_with(|_| {}).await
}

pub async fn h_with(tweak: impl FnOnce(&mut Config)) -> H {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = Config::for_tests(dir.path().to_path_buf());
    tweak(&mut cfg);
    let tg = Arc::new(FakeTelegram::default());
    let db = quiz_forge::db::memory().await.unwrap();
    let app = App::new(db, cfg, tg.clone());
    let now = Utc.with_ymd_and_hms(2026, 10, 1, 12, 0, 0).unwrap();
    questions::seed_themes(&app, now).await.unwrap();
    H { app, tg, dir, now }
}

impl H {
    pub fn tick(&mut self, d: Duration) -> DateTime<Utc> {
        self.now += d;
        self.now
    }

    pub async fn member(&self, tg_id: i64, name: &str) -> Member {
        members::upsert_telegram(&self.app, tg_id, name, None, self.now)
            .await
            .unwrap()
    }

    pub async fn deliver(&self) -> usize {
        outbox::deliver_due(&self.app, self.now).await.unwrap()
    }

    pub fn group_texts(&self) -> Vec<String> {
        self.tg
            .sent_texts()
            .into_iter()
            .filter(|(c, _)| *c == GROUP)
            .map(|(_, t)| t)
            .collect()
    }

    pub fn dm_texts(&self, tg_id: i64) -> Vec<String> {
        self.tg
            .sent_texts()
            .into_iter()
            .filter(|(c, _)| *c == tg_id)
            .map(|(_, t)| t)
            .collect()
    }

    pub async fn submit(&self, author: &Member, c: Content) -> SubmitOutcome {
        questions::submit(&self.app, author, c, true, self.now)
            .await
            .unwrap()
    }

    pub async fn submitted(&self, author: &Member, c: Content) -> i64 {
        match self.submit(author, c).await {
            SubmitOutcome::Submitted(id) => id,
            other => panic!("expected a submission, got {other:?}"),
        }
    }

    /// Submits and gets one approval from `voter`.
    pub async fn accepted(&self, author: &Member, voter: &Member, c: Content) -> i64 {
        let id = self.submitted(author, c).await;
        questions::vote(
            &self.app,
            id,
            None,
            voter,
            quiz_forge::domain::voting::Vote::Yes,
            self.now,
        )
        .await
        .unwrap();
        id
    }

    pub async fn status(&self, qid: i64) -> String {
        sqlx::query_scalar("SELECT status FROM question WHERE id = ?")
            .bind(qid)
            .fetch_one(&self.app.db)
            .await
            .unwrap()
    }

    pub async fn count(&self, sql: &'static str) -> i64 {
        sqlx::query_scalar(sql)
            .fetch_one(&self.app.db)
            .await
            .unwrap()
    }
}

pub fn text(prompt: &str, answer: &str) -> Content {
    Content::text(prompt, answer, "geography", "fi")
}

pub fn themed(prompt: &str, answer: &str, theme: &str, kind: Kind, difficulty: u8) -> Content {
    let mut c = Content::text(prompt, answer, theme, "fi");
    c.kind = kind;
    c.difficulty = difficulty;
    c
}

pub fn image_q(prompt: &str, answer: &str, theme: &str, media_id: i64) -> Content {
    let mut c = themed(prompt, answer, theme, Kind::Image, 3);
    c.media = vec![MediaRef {
        media_id,
        kind: MediaKind::Image,
        start_s: None,
        end_s: None,
    }];
    c
}

pub const PNG: &[u8] = &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A, 1, 2, 3];
