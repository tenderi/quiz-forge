//! The web app through its router: auth, CSRF, forms, pages and printouts.

mod common;

use axum::body::Body;
use axum::http::{Request, StatusCode, header};
use common::*;
use http_body_util::BodyExt;
use quiz_forge::service::members::Member;
use quiz_forge::service::{App, auth, questions, quizzes};
use std::sync::Arc;
use tower::ServiceExt;

struct Client {
    app: App,
    cookie: String,
    csrf: String,
}

struct Res {
    status: StatusCode,
    headers: axum::http::HeaderMap,
    body: String,
    bytes: Vec<u8>,
}

impl Res {
    fn header(&self, k: &str) -> String {
        self.headers
            .get(k)
            .map(|v| v.to_str().unwrap().to_string())
            .unwrap_or_default()
    }
}

async fn send(app: &App, req: Request<Body>) -> Res {
    let res = quiz_forge::web::router(app.clone())
        .oneshot(req)
        .await
        .unwrap();
    let status = res.status();
    let headers = res.headers().clone();
    let bytes = res.into_body().collect().await.unwrap().to_bytes().to_vec();
    Res {
        status,
        headers,
        body: String::from_utf8_lossy(&bytes).to_string(),
        bytes,
    }
}

fn fixed_clock(h: &mut H) {
    let now = h.now;
    h.app.clock = Arc::new(move || now);
}

async fn login(h: &H, m: &Member) -> Client {
    let token = auth::create_login_token(&h.app, m, h.now).await.unwrap();
    let res = send(
        &h.app,
        Request::get(format!("/login/{token}"))
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    let set = res.header("set-cookie");
    assert!(set.contains("HttpOnly") && set.contains("SameSite=Lax"));
    let cookie = set.split(';').next().unwrap().to_string();
    let raw = cookie.split_once('=').unwrap().1;
    let s = auth::session(&h.app, raw, h.now).await.unwrap().unwrap();
    Client {
        app: h.app.clone(),
        cookie,
        csrf: s.csrf,
    }
}

impl Client {
    async fn get(&self, path: &str) -> Res {
        send(
            &self.app,
            Request::get(path)
                .header(header::COOKIE, &self.cookie)
                .body(Body::empty())
                .unwrap(),
        )
        .await
    }

    async fn post_form(&self, path: &str, form: &[(&str, &str)]) -> Res {
        let body = url::form_urlencoded::Serializer::new(String::new())
            .extend_pairs(form)
            .finish();
        let req = Request::post(path)
            .header(header::COOKIE, &self.cookie)
            .header("x-csrf-token", &self.csrf)
            .header("hx-request", "true")
            .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
            .body(Body::from(body))
            .unwrap();
        send(&self.app, req).await
    }

    async fn post_multipart(&self, path: &str, fields: &[(&str, &str)], files: &[&[u8]]) -> Res {
        let b = "XBOUNDARYX";
        let mut body: Vec<u8> = Vec::new();
        for (k, v) in fields {
            body.extend(
                format!("--{b}\r\nContent-Disposition: form-data; name=\"{k}\"\r\n\r\n{v}\r\n")
                    .as_bytes(),
            );
        }
        for (i, f) in files.iter().enumerate() {
            body.extend(
                format!("--{b}\r\nContent-Disposition: form-data; name=\"files\"; filename=\"f{i}\"\r\nContent-Type: application/octet-stream\r\n\r\n")
                    .as_bytes(),
            );
            body.extend(*f);
            body.extend(b"\r\n");
        }
        body.extend(format!("--{b}--\r\n").as_bytes());
        let req = Request::post(path)
            .header(header::COOKIE, &self.cookie)
            .header("x-csrf-token", &self.csrf)
            .header("hx-request", "true")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={b}"),
            )
            .body(Body::from(body))
            .unwrap();
        send(&self.app, req).await
    }
}

fn q_fields<'a>(prompt: &'a str, answer: &'a str) -> Vec<(&'a str, &'a str)> {
    vec![
        ("kind", "text"),
        ("prompt", prompt),
        ("answer", answer),
        ("difficulty", "2"),
        ("theme", "geography"),
        ("language", "fi"),
    ]
}

#[tokio::test]
async fn anonymous_visitors_are_sent_to_login() {
    let h = h().await;
    let res = send(&h.app, Request::get("/").body(Body::empty()).unwrap()).await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
    assert_eq!(res.header("location"), "/login");
    let res = send(
        &h.app,
        Request::get("/questions")
            .header("hx-request", "true")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::UNAUTHORIZED);
    assert_eq!(res.header("hx-redirect"), "/login");
    let res = send(&h.app, Request::get("/login").body(Body::empty()).unwrap()).await;
    assert!(res.body.contains("/web"));
    let res = send(
        &h.app,
        Request::get("/login/nonsense").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    assert!(res.body.contains("isn't valid"));
    let res = send(
        &h.app,
        Request::get("/media/1").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER, "media needs a session");
    let res = send(
        &h.app,
        Request::post("/questions").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(res.status, StatusCode::SEE_OTHER);
}

#[tokio::test]
async fn static_files_health_and_headers() {
    let h = h().await;
    let res = send(
        &h.app,
        Request::get("/healthz").body(Body::empty()).unwrap(),
    )
    .await;
    assert_eq!(res.body, "ok");
    assert!(
        res.header("content-security-policy")
            .contains("script-src 'self'")
    );
    assert_eq!(res.header("x-content-type-options"), "nosniff");
    let js = send(
        &h.app,
        Request::get("/static/htmx.min.js")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(js.body.starts_with("var htmx"));
    let css = send(
        &h.app,
        Request::get("/static/app.css").body(Body::empty()).unwrap(),
    )
    .await;
    assert!(css.header("content-type").starts_with("text/css"));
}

#[tokio::test]
async fn csrf_is_required_for_posts() {
    let mut h = h().await;
    fixed_clock(&mut h);
    let a = h.member(1, "Aino").await;
    let c = login(&h, &a).await;
    let req = Request::post("/themes")
        .header(header::COOKIE, &c.cookie)
        .header(header::CONTENT_TYPE, "application/x-www-form-urlencoded")
        .body(Body::from("name=x"))
        .unwrap();
    let res = send(&h.app, req).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = c.post_form("/themes", &[("name", "Board Games")]).await;
    assert_eq!(res.status, StatusCode::OK);
    assert!(res.header("hx-redirect").starts_with("/questions?notice="));
    assert!(
        questions::themes(&h.app)
            .await
            .unwrap()
            .contains(&"board games".into())
    );
}

#[tokio::test]
async fn submit_vote_and_propose_through_the_web() {
    let mut h = h().await;
    fixed_clock(&mut h);
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    let ca = login(&h, &a).await;
    let cb = login(&h, &b).await;

    let home = ca.get("/").await;
    assert_eq!(home.status, StatusCode::OK);
    assert!(home.body.contains("Hi, Aino"));
    assert!(home.body.contains(&ca.csrf), "token in hx-headers");
    let form = ca.get("/questions/new").await;
    assert!(form.body.contains("Submit a question"));

    // live check
    let fb = ca
        .post_multipart(
            "/questions/check",
            &q_fields("Onko tämä totta?", "Kyllä"),
            &[],
        )
        .await;
    assert!(fb.body.contains("Yes/no questions"));
    let empty = ca
        .post_multipart("/questions/check", &[("prompt", "")], &[])
        .await;
    assert_eq!(empty.body, "");

    // refused
    let res = ca
        .post_multipart("/questions", &q_fields("Onko tämä totta?", "Kyllä"), &[])
        .await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    assert!(res.body.contains("Yes/no questions"));

    // accepted for voting
    let res = ca
        .post_multipart(
            "/questions",
            &q_fields("Mikä on Suomen pääkaupunki?", "Helsinki"),
            &[],
        )
        .await;
    assert_eq!(
        res.header("hx-redirect"),
        "/questions/1?notice=Submitted.+The+group+can+vote+now."
    );

    // duplicate needs confirmation
    let res = ca
        .post_multipart(
            "/questions",
            &q_fields("Mikä on Suomen pääkaupunki?", "Helsinki"),
            &[],
        )
        .await;
    assert!(res.body.contains("Similar questions already exist"));
    assert!(res.body.contains("name=\"confirm\""));

    // detail: author can't vote, Bo can
    let page = ca.get("/questions/1?notice=hello").await;
    assert!(page.body.contains("hello"));
    assert!(
        page.body.contains("<details class=\"answer\""),
        "answer collapsed"
    );
    assert!(!page.body.contains("/questions/1/vote"));
    let page = cb.get("/questions/1").await;
    assert!(page.body.contains("/questions/1/vote"));
    assert!(page.body.contains("Propose a change"));
    let res = cb
        .post_form("/questions/1/vote", &[("value", "maybe")])
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = cb
        .post_form("/questions/1/vote", &[("value", "yes"), ("round", "1")])
        .await;
    assert!(res.header("hx-redirect").contains("accepted"));
    assert_eq!(h.status(1).await, "accepted");

    // Bo proposes, Aino accepts
    let edit = cb.get("/questions/1/edit").await;
    assert!(edit.body.contains("Propose a change to"));
    let mut f = q_fields("Mikä on Suomen pääkaupunki?", "Helsinki");
    f.push(("fun_fact", "Perustettu 1550."));
    f.push(("rationale", "fun fact"));
    let res = cb.post_multipart("/questions/1/edit", &f, &[]).await;
    assert!(res.header("hx-redirect").contains("Proposal+%231+filed"));
    let page = ca.get("/questions/1").await;
    assert!(page.body.contains("proposal-1"));
    assert!(page.body.contains("<ins>Perustettu"));
    let res = ca
        .post_form("/proposals/1/decide", &[("accept", "true")])
        .await;
    assert!(res.header("hx-redirect").contains("Proposal+accepted"));
    let page = ca.get("/questions/1").await;
    assert!(page.body.contains("Version 2"));

    // author edits: no change, then a minor one
    let res = ca.post_multipart("/questions/1/edit", &f, &[]).await;
    assert!(res.header("hx-redirect").contains("Nothing+changed"));
    f.push(("tags", "capitals"));
    let res = ca.post_multipart("/questions/1/edit", &f, &[]).await;
    assert!(res.header("hx-redirect").contains("Saved."));

    // withdraw and decline paths
    let mut g = f.clone();
    g.push(("host_notes", "say it slowly"));
    cb.post_multipart("/questions/1/edit", &g, &[]).await;
    let res = ca.post_form("/proposals/2/withdraw", &[]).await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
    let res = cb.post_form("/proposals/2/withdraw", &[]).await;
    assert!(res.header("hx-redirect").contains("withdrawn"));

    // notes, retire, bring back
    let res = cb
        .post_form("/questions/1/notes", &[("body", "Nice one")])
        .await;
    assert_eq!(res.header("hx-redirect"), "/questions/1");
    assert!(ca.get("/questions/1").await.body.contains("Nice one"));
    assert!(
        cb.post_form("/questions/1/retire", &[("retired", "true")])
            .await
            .header("hx-redirect")
            .contains("Retired")
    );
    assert!(
        cb.post_form("/questions/1/retire", &[("retired", "false")])
            .await
            .header("hx-redirect")
            .contains("Back+in+use")
    );
    let res = ca.post_form("/questions/1/resubmit", &[]).await;
    assert_eq!(res.status, StatusCode::CONFLICT);

    // the bank and its filters
    let bank = ca.get("/questions?q=pääkaupunki&status=accepted&theme=geography&kind=text&difficulty=2&language=fi&mine=1").await;
    assert!(bank.body.contains("Mikä on Suomen pääkaupunki?"));
    let none = ca.get("/questions?q=zzz").await;
    assert!(none.body.contains("No questions match"));
    let vote = cb.get("/questions?needs_vote=1").await;
    assert!(vote.body.contains("Waiting for your vote"));
    assert!(
        ca.get("/questions?flagged=1&page=1")
            .await
            .body
            .contains("Newer")
    );
    assert_eq!(ca.get("/questions/99").await.status, StatusCode::NOT_FOUND);
}

#[tokio::test]
async fn non_members_cannot_submit() {
    let mut h = h().await;
    fixed_clock(&mut h);
    let a = h.member(1, "Aino").await;
    let c = login(&h, &a).await;
    h.tg.non_members.lock().unwrap().push(1);
    sqlx::query("UPDATE member SET checked_at = NULL")
        .execute(&h.app.db)
        .await
        .unwrap();
    let res = c
        .post_multipart("/questions", &q_fields("Pisin joki?", "Niili"), &[])
        .await;
    assert_eq!(res.status, StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn image_upload_and_media_serving() {
    let mut h = h().await;
    fixed_clock(&mut h);
    let a = h.member(1, "Aino").await;
    let c = login(&h, &a).await;
    let mut f = q_fields("Kuka on kuvassa?", "Sibelius");
    f[0] = ("kind", "image");
    f.push(("new_theme", "Composers"));
    let res = c.post_multipart("/questions", &f, &[PNG]).await;
    assert!(
        res.header("hx-redirect").starts_with("/questions/1"),
        "{}",
        res.body
    );
    let d = questions::detail(&h.app, 1).await.unwrap();
    assert_eq!(d.current.content.theme, "composers");
    let mid = d.current.content.media[0].media_id;
    let img = c.get(&format!("/media/{mid}")).await;
    assert_eq!(img.header("content-type"), "image/png");
    assert_eq!(img.bytes, PNG);
    assert_eq!(c.get("/media/999").await.status, StatusCode::NOT_FOUND);
    let edit = c.get("/questions/1/edit").await;
    assert!(edit.body.contains(&format!("value=\"{mid}:image\"")));
    let res = c
        .post_multipart(
            "/questions",
            &q_fields("Bad file?", "x"),
            &[b"not an image"],
        )
        .await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
}

#[tokio::test]
async fn templates_quiz_builder_prints_events_players() {
    let mut h = h().await;
    fixed_clock(&mut h);
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    for (i, r) in ["Niili", "Amazon", "Jangtse", "Lena"].iter().enumerate() {
        h.accepted(
            &a,
            &b,
            text(&format!("Kysymys {i}: joki numero {}?", i * 13), r),
        )
        .await;
    }
    let c = login(&h, &a).await;

    assert!(c.get("/templates").await.body.contains("New template"));
    assert!(
        c.get("/templates/new")
            .await
            .body
            .contains("Standard pub night")
    );
    let chk = c
        .post_form(
            "/templates/check",
            &[(
                "body",
                "name = \"x\"\n[[round]]\ntitle = \"R\"\n[[round.slot]]\ncount = 2",
            )],
        )
        .await;
    assert!(chk.body.contains("1 rounds, 2 questions"));
    let bad = c.post_form("/templates/check", &[("body", "nope")]).await;
    assert!(bad.body.contains("not valid TOML"));
    let res = c.post_form("/templates", &[("body", "name = \"\"")]).await;
    assert_eq!(res.status, StatusCode::UNPROCESSABLE_ENTITY);
    let tpl = "name = \"Mini\"\n[[round]]\ntitle = \"Rivers\"\n[[round.slot]]\ncount = 3\n";
    let res = c.post_form("/templates", &[("body", tpl)]).await;
    assert!(res.header("hx-redirect").starts_with("/templates/1"));
    assert!(
        c.get("/templates/1")
            .await
            .body
            .contains("Make a quiz from it")
    );
    let res = c
        .post_form("/templates", &[("id", "1"), ("body", tpl)])
        .await;
    assert!(res.header("hx-redirect").contains("saved"));

    // players
    assert!(
        c.post_form("/players/import", &[])
            .await
            .header("hx-redirect")
            .contains("2+player")
    );
    assert!(
        c.post_form("/players", &[("name", "Guest"), ("member_id", "")])
            .await
            .header("hx-redirect")
            .contains("added")
    );
    assert!(c.get("/players").await.body.contains("Guest"));

    // quiz
    assert!(c.get("/quizzes").await.body.contains("Build quiz"));
    let res = c.post_form("/quizzes", &[("template_id", "x")]).await;
    assert_eq!(res.status, StatusCode::BAD_REQUEST);
    let res = c
        .post_form(
            "/quizzes",
            &[("template_id", "1"), ("name", "Friday"), ("seed", "abc")],
        )
        .await;
    assert_eq!(res.header("hx-redirect"), "/quizzes/1");
    let page = c.get("/quizzes/1").await;
    assert!(page.body.contains("Friday") && page.body.contains("3/3 filled"));
    let opts = c.get("/quizzes/1/slots/0/0/options").await;
    assert!(opts.body.contains("Use"));
    let v = quizzes::view(&h.app, 1).await.unwrap();
    let spare = (1..=4)
        .find(|id| !v.question_ids().contains(id))
        .unwrap()
        .to_string();
    assert!(
        c.post_form("/quizzes/1/slots/0/0/set", &[("question_id", &spare)])
            .await
            .header("hx-redirect")
            .contains("#slot-0-0")
    );
    c.post_form("/quizzes/1/slots/0/0/unlock", &[]).await;
    c.post_form("/quizzes/1/slots/0/0/lock", &[]).await;
    c.post_form("/quizzes/1/slots/0/1/clear", &[]).await;
    assert_eq!(
        c.post_form("/quizzes/1/slots/0/1/explode", &[])
            .await
            .status,
        StatusCode::NOT_FOUND
    );
    assert_eq!(
        c.post_form("/quizzes/1/freeze", &[]).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        c.get("/quizzes/1/print/host").await.status,
        StatusCode::CONFLICT,
        "not frozen"
    );
    assert!(
        c.post_form("/quizzes/1/refill", &[("seed", "")])
            .await
            .header("hx-redirect")
            .contains("Refilled")
    );
    assert!(
        c.post_form("/quizzes/1/players", &[("players", "3")])
            .await
            .header("hx-redirect")
            .contains("Players+saved")
    );
    assert!(
        c.post_form("/quizzes/1/freeze", &[])
            .await
            .header("hx-redirect")
            .contains("Frozen")
    );

    let host = c.get("/quizzes/1/print/host").await;
    assert!(host.body.contains("Friday — Rivers") && host.body.contains("→ "));
    let sheets = c.get("/quizzes/1/print/sheets").await;
    assert!(sheets.body.contains("Team:") && !sheets.body.contains("Niili"));
    assert!(
        c.get("/quizzes/1/print/pictures")
            .await
            .body
            .contains("no picture questions")
    );
    let present = c.get("/quizzes/1/present?i=1").await;
    assert!(present.body.contains("question 1.2 of 3") && present.body.contains("Previous"));
    let csv = c.get("/quizzes/1/answers.csv").await;
    assert!(csv.header("content-type").starts_with("text/csv"));
    assert!(
        csv.body
            .starts_with("number,round,question,answer,also_accepted,points\n1.1,Rivers,")
    );
    assert_eq!(csv.body.lines().count(), 4);

    // events
    let qpage = c.get("/quizzes/1").await;
    assert!(qpage.body.contains("Schedule an event"));
    assert!(
        c.post_form(
            "/events",
            &[("quiz_id", "1"), ("date", "2026-10-31"), ("players", "3")]
        )
        .await
        .header("hx-redirect")
        .contains("Event+created")
    );
    let ev = c.get("/events").await;
    assert!(ev.body.contains("2026-10-31") && ev.body.contains("Mark as held"));
    assert!(
        c.post_form("/events/1/held", &[])
            .await
            .header("hx-redirect")
            .contains("held")
    );
    assert!(c.get("/events").await.body.contains("held"));
    assert_eq!(
        c.post_form("/quizzes/1/unfreeze", &[]).await.status,
        StatusCode::CONFLICT
    );
    assert_eq!(
        c.post_form("/quizzes/1/delete", &[]).await.status,
        StatusCode::CONFLICT
    );

    // a second quiz can be unfrozen and deleted; template deletion keeps quizzes
    c.post_form("/quizzes", &[("template_id", "1"), ("seed", "z")])
        .await;
    assert!(
        c.post_form("/quizzes/2/unfreeze", &[])
            .await
            .header("hx-redirect")
            .contains("Unfrozen")
    );
    assert!(
        c.post_form("/quizzes/2/delete", &[])
            .await
            .header("hx-redirect")
            .contains("deleted")
    );
    assert!(
        c.post_form("/templates/1/delete", &[])
            .await
            .header("hx-redirect")
            .contains("deleted")
    );
    assert!(c.get("/quizzes").await.body.contains("Friday"));
}

#[tokio::test]
async fn logout_ends_the_session() {
    let mut h = h().await;
    fixed_clock(&mut h);
    let a = h.member(1, "Aino").await;
    let c = login(&h, &a).await;
    let res = c.post_form("/logout", &[]).await;
    assert!(res.header("set-cookie").contains("Max-Age=0"));
    assert_eq!(c.get("/").await.status, StatusCode::SEE_OTHER);
}
