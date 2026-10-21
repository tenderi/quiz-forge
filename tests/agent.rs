//! The review agent: the run loop with a fake provider, the Claude client
//! against a local mock of the Messages API, and the scheduler's gating.

mod common;

use async_trait::async_trait;
use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use chrono::Duration;
use common::*;
use quiz_forge::agent::anthropic::{self, Anthropic, Usage};
use quiz_forge::agent::{self, ProviderOutput, ReviewInput, ReviewProvider};
use quiz_forge::config::AgentConfig;
use quiz_forge::service::questions;
use quiz_forge::service::reviews::{ReviewResult, Verdict};
use serde_json::{Value, json};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

struct Fake {
    cost: f64,
    fail_on: Option<i64>,
    seen: Mutex<Vec<i64>>,
    images: Mutex<usize>,
}

fn ok_result() -> ReviewResult {
    ReviewResult {
        verdict: Verdict::Ok,
        explanation: "Correct.".into(),
        sources: vec![],
        typo_fixes: None,
        corrected_answer: None,
        fun_fact: Some("A fact.".into()),
        background: None,
        difficulty: None,
        tags: None,
        flags: vec![],
        duplicate_of: vec![],
    }
}

#[async_trait]
impl ReviewProvider for Fake {
    fn name(&self) -> &str {
        "fake"
    }
    fn model(&self) -> &str {
        "fake-1"
    }
    async fn review(&self, input: &ReviewInput) -> anyhow::Result<ProviderOutput> {
        self.seen.lock().unwrap().push(input.question_id);
        *self.images.lock().unwrap() += input.images.len();
        if self.fail_on == Some(input.question_id) {
            anyhow::bail!("boom");
        }
        Ok(ProviderOutput {
            result: ok_result(),
            usage: Usage {
                tokens_in: 10,
                tokens_out: 5,
                cost_usd: self.cost,
            },
            model: "fake-1".into(),
        })
    }
}

async fn bank(h: &H, n: usize) -> Vec<i64> {
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    let mut ids = Vec::new();
    for i in 0..n {
        ids.push(
            h.accepted(
                &a,
                &b,
                text(
                    &format!("Kysymys numero {i} joesta {}?", i * 31),
                    &format!("Vastaus{i}"),
                ),
            )
            .await,
        );
    }
    ids
}

#[tokio::test]
async fn run_once_reviews_records_and_respects_caps() {
    let mut h = h_with(|c| {
        c.agent.max_questions_per_run = 3;
        c.agent.max_cost_usd_per_run = 0.25;
    })
    .await;
    let now = h.now;
    h.app.clock = Arc::new(move || now);
    let ids = bank(&h, 5).await;
    let fake = Fake {
        cost: 0.1,
        fail_on: Some(ids[1]),
        seen: Mutex::new(vec![]),
        images: Mutex::new(0),
    };
    let s = agent::run_once(&h.app, &fake).await.unwrap();
    assert_eq!(
        *fake.seen.lock().unwrap(),
        vec![ids[0], ids[1], ids[2]],
        "max 3 per run"
    );
    assert_eq!((s.reviewed, s.failed), (2, 1));
    assert!(!s.stopped_on_budget);
    let run: (i64, Option<String>, f64) =
        sqlx::query_as("SELECT questions, error, cost_usd FROM review_run WHERE id = ?")
            .bind(s.run_id)
            .fetch_one(&h.app.db)
            .await
            .unwrap();
    assert_eq!(run.0, 2);
    assert!(run.1.unwrap().contains("boom"));
    assert!((run.2 - 0.2).abs() < 1e-9);
    // the fun fact became an agent proposal
    let d = questions::detail(&h.app, ids[0]).await.unwrap();
    assert_eq!(d.proposals[0].proposer_name, "Review agent");

    // cost cap: 0.1 per question, cap 0.25 → three, then stop
    let fake = Fake {
        cost: 0.1,
        fail_on: None,
        seen: Mutex::new(vec![]),
        images: Mutex::new(0),
    };
    h.tick(Duration::hours(1));
    let s = agent::run_once(&h.app, &fake).await.unwrap();
    assert_eq!(s.reviewed, 3);
    assert!(
        !s.stopped_on_budget,
        "only 3 pending, cap reached exactly at the end"
    );
    bank(&h, 3).await;
    let fake = Fake {
        cost: 1.0,
        fail_on: None,
        seen: Mutex::new(vec![]),
        images: Mutex::new(0),
    };
    let s = agent::run_once(&h.app, &fake).await.unwrap();
    assert_eq!(
        s.reviewed, 1,
        "the first review already costs more than the cap"
    );
    assert!(s.stopped_on_budget);
}

#[tokio::test]
async fn scheduler_gates_agent_runs_by_interval() {
    let mut h = h().await;
    let now = h.now;
    h.app.clock = Arc::new(move || now);
    bank(&h, 1).await;
    let provider: Arc<dyn ReviewProvider> = Arc::new(Fake {
        cost: 0.0,
        fail_on: None,
        seen: Mutex::new(vec![]),
        images: Mutex::new(0),
    });
    let running = Arc::new(AtomicBool::new(false));
    assert!(agent::due(&h.app, h.now).await.unwrap());
    assert!(
        quiz_forge::scheduler::maybe_review(&h.app, &provider, &running)
            .await
            .unwrap()
    );
    // wait for the spawned run
    for _ in 0..100 {
        if !running.load(Ordering::SeqCst) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert_eq!(h.count("SELECT count(*) FROM review").await, 1);
    assert!(
        !quiz_forge::scheduler::maybe_review(&h.app, &provider, &running)
            .await
            .unwrap(),
        "not due again yet"
    );
    assert!(
        agent::due(&h.app, h.now + Duration::hours(24))
            .await
            .unwrap()
    );
    running.store(true, Ordering::SeqCst);
    h.tick(Duration::hours(25));
    let later = h.now;
    h.app.clock = Arc::new(move || later);
    assert!(
        !quiz_forge::scheduler::maybe_review(&h.app, &provider, &running)
            .await
            .unwrap(),
        "already running"
    );

    // a plain tick expires votes and delivers
    let a = h.member(5, "Eve").await;
    h.submitted(&a, text("Pisin joki maailmassa?", "Niili"))
        .await;
    h.tick(Duration::days(8));
    let later = h.now;
    h.app.clock = Arc::new(move || later);
    quiz_forge::scheduler::tick(&h.app).await.unwrap();
    assert_eq!(
        h.count("SELECT count(*) FROM question WHERE status = 'rejected'")
            .await,
        1
    );
    assert!(
        h.count("SELECT count(*) FROM outbox WHERE sent_at IS NULL")
            .await
            == 0
    );
}

// ---------------------------------------------------------------- mock API

#[derive(Clone, Default)]
struct Mock {
    replies: Arc<Mutex<Vec<(u16, Value)>>>,
    requests: Arc<Mutex<Vec<(HeaderMap, Value)>>>,
}

async fn messages(
    State(m): State<Mock>,
    headers: HeaderMap,
    Json(body): Json<Value>,
) -> (axum::http::StatusCode, Json<Value>) {
    m.requests.lock().unwrap().push((headers, body));
    let (status, reply) = m.replies.lock().unwrap().remove(0);
    (
        axum::http::StatusCode::from_u16(status).unwrap(),
        Json(reply),
    )
}

async fn mock(replies: Vec<(u16, Value)>) -> (Mock, String) {
    let m = Mock {
        replies: Arc::new(Mutex::new(replies)),
        ..Default::default()
    };
    let app = axum::Router::new()
        .route("/v1/messages", axum::routing::post(messages))
        .with_state(m.clone());
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    (m, url)
}

fn usage(i: u64, o: u64) -> Value {
    json!({"input_tokens": i, "output_tokens": o, "server_tool_use": {"web_search_requests": 1}})
}

fn submit(verdict: &str) -> Value {
    json!({"type": "tool_use", "id": "toolu_1", "name": "submit_review", "input": {
        "verdict": verdict, "explanation": "Checked the atlas.", "sources": ["https://example.org"],
        "typo_fixes": {"prompt": "Mikä on pisin joki?", "background": null, "fun_fact": null, "host_notes": null},
        "corrected_answer": null, "fun_fact": null, "background": null, "difficulty": 2,
        "tags": ["rivers"], "flags": [], "duplicate_of": []
    }})
}

fn input() -> ReviewInput {
    ReviewInput {
        question_id: 1,
        version_id: 1,
        content: quiz_forge::domain::model::Content::text(
            "Mikä on pisn joki?",
            "Niili",
            "geography",
            "fi",
        ),
        neighbours: vec![],
        images: vec![],
    }
}

fn client(url: &str) -> Anthropic {
    Anthropic::new(AgentConfig {
        enabled: true,
        api_key: Some("test-key".into()),
        api_url: url.into(),
        ..Default::default()
    })
}

#[tokio::test]
async fn claude_client_resumes_nudges_and_submits() {
    let (m, url) = mock(vec![
        (200, json!({"model": "claude-opus-5-5", "stop_reason": "pause_turn", "usage": usage(1000, 100),
            "content": [{"type": "server_tool_use", "id": "srv_1", "name": "web_search", "input": {"query": "pisin joki"}}]})),
        (200, json!({"model": "claude-opus-5-5", "stop_reason": "end_turn", "usage": usage(1000, 100),
            "content": [{"type": "text", "text": "The Nile is generally considered the longest."}]})),
        (200, json!({"model": "claude-opus-5-5", "stop_reason": "tool_use", "usage": usage(1000, 100),
            "content": [submit("ok")]})),
    ])
    .await;
    let out = client(&url).review(&input()).await.unwrap();
    assert_eq!(out.result.verdict, Verdict::Ok);
    assert_eq!(
        out.result.typo_fixes.unwrap().prompt.as_deref(),
        Some("Mikä on pisin joki?")
    );
    assert_eq!(out.usage.tokens_in, 3000);
    assert!((out.usage.cost_usd - (3000.0 * 4.0 / 1e6 + 300.0 * 20.0 / 1e6 + 0.03)).abs() < 1e-9);
    let reqs = m.requests.lock().unwrap().clone();
    assert_eq!(reqs.len(), 3);
    let (h0, b0) = &reqs[0];
    assert_eq!(h0["x-api-key"], "test-key");
    assert_eq!(h0["anthropic-version"], anthropic::API_VERSION);
    assert_eq!(h0["anthropic-beta"], anthropic::FALLBACK_BETA);
    assert_eq!(b0["messages"].as_array().unwrap().len(), 1);
    // pause_turn: the paused assistant turn goes back as is, nothing else added
    let m1 = reqs[1].1["messages"].as_array().unwrap();
    assert_eq!(m1.len(), 2);
    assert_eq!(m1[1]["role"], "assistant");
    assert_eq!(m1[1]["content"][0]["type"], "server_tool_use");
    // no submission: one nudge
    let m2 = reqs[2].1["messages"].as_array().unwrap();
    assert_eq!(m2.len(), 4);
    assert_eq!(m2[3]["role"], "user");
}

#[tokio::test]
async fn claude_client_errors_and_refusals() {
    let (_, url) = mock(vec![(
        529,
        json!({"type": "error", "error": {"type": "overloaded_error", "message": "Overloaded"}}),
    )])
    .await;
    let err = client(&url).review(&input()).await.unwrap_err().to_string();
    assert!(err.contains("529") && err.contains("Overloaded"), "{err}");

    let (_, url) = mock(vec![(200, json!({"model": "claude-opus-5-5", "stop_reason": "refusal", "stop_details": {"category": "bio"}, "usage": usage(10, 0), "content": []}))]).await;
    let out = client(&url).review(&input()).await.unwrap();
    assert_eq!(out.result.verdict, Verdict::Unverifiable);
    assert!(out.result.explanation.contains("bio"));

    let end = json!({"model": "claude-opus-5-5", "stop_reason": "end_turn", "usage": usage(10, 10), "content": [{"type": "text", "text": "hm"}]});
    let (_, url) = mock(vec![(200, end.clone()), (200, end)]).await;
    assert!(
        client(&url)
            .review(&input())
            .await
            .unwrap_err()
            .to_string()
            .contains("without calling submit_review")
    );

    let pause = json!({"model": "claude-opus-5-5", "stop_reason": "pause_turn", "usage": usage(1, 1), "content": []});
    let (_, url) = mock(vec![(200, pause); anthropic::MAX_CONTINUATIONS + 1]).await;
    assert!(
        client(&url)
            .review(&input())
            .await
            .unwrap_err()
            .to_string()
            .contains("gave up")
    );

    let no_key = Anthropic::new(AgentConfig::default());
    assert!(
        no_key
            .review(&input())
            .await
            .unwrap_err()
            .to_string()
            .contains("ANTHROPIC_API_KEY")
    );
    assert_eq!(no_key.name(), "anthropic");
    assert_eq!(no_key.model(), "claude-opus-5-5");
    assert_eq!(agent::declined("x").flags, vec!["not_reviewed".to_string()]);
}

#[tokio::test]
async fn images_are_sent_only_when_allowed() {
    let h = h_with(|c| c.agent.send_media = true).await;
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    let m = quiz_forge::service::media::store(&h.app, PNG, a.id, h.now)
        .await
        .unwrap();
    let qid = h
        .accepted(
            &a,
            &b,
            image_q("Kuka on kuvassa?", "Sibelius", "music", m.id),
        )
        .await;
    let fake = Fake {
        cost: 0.0,
        fail_on: None,
        seen: Mutex::new(vec![]),
        images: Mutex::new(0),
    };
    agent::run_once(&h.app, &fake).await.unwrap();
    assert_eq!(*fake.seen.lock().unwrap(), vec![qid]);
    assert_eq!(*fake.images.lock().unwrap(), 1);

    let off = h_with(|_| {}).await;
    let a = off.member(1, "Aino").await;
    let b = off.member(2, "Bo").await;
    let m = quiz_forge::service::media::store(&off.app, PNG, a.id, off.now)
        .await
        .unwrap();
    off.accepted(
        &a,
        &b,
        image_q("Kuka on kuvassa?", "Sibelius", "music", m.id),
    )
    .await;
    let fake = Fake {
        cost: 0.0,
        fail_on: None,
        seen: Mutex::new(vec![]),
        images: Mutex::new(0),
    };
    agent::run_once(&off.app, &fake).await.unwrap();
    assert_eq!(
        *fake.images.lock().unwrap(),
        0,
        "AGENT_SEND_MEDIA is off by default"
    );
}
