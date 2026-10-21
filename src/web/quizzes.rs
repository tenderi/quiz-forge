//! Templates, the quiz builder, events and players.

use super::layout::{Nav, answer_block, meta_chips, page, status_badge};
use super::questions::NoticeQ;
use super::{Viewer, WebResult, go, with_notice};
use crate::domain::quizfill::Template;
use crate::service::quizzes::{self, SlotView};
use crate::service::{App, AppError, members};
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/templates", get(templates).post(save_template))
        .route("/templates/new", get(new_template))
        .route("/templates/check", post(check_template))
        .route("/templates/{id}", get(edit_template))
        .route("/templates/{id}/delete", post(delete_template))
        .route("/quizzes", get(quizzes_page).post(create_quiz))
        .route("/quizzes/{id}", get(quiz_page))
        .route("/quizzes/{id}/refill", post(refill))
        .route("/quizzes/{id}/players", post(set_players))
        .route("/quizzes/{id}/freeze", post(freeze))
        .route("/quizzes/{id}/unfreeze", post(unfreeze))
        .route("/quizzes/{id}/delete", post(delete_quiz))
        .route("/quizzes/{id}/slots/{r}/{s}/options", get(slot_options))
        .route("/quizzes/{id}/slots/{r}/{s}/{action}", post(slot_action))
        .route("/events", get(events_page).post(create_event))
        .route("/events/{id}/held", post(mark_held))
        .route("/players", get(players_page).post(add_player))
        .route("/players/import", post(import_players))
}

fn nav<'a>(v: &'a Viewer, active: &'a str, notice: Option<&'a str>) -> Nav<'a> {
    Nav {
        me: v.member(),
        csrf: &v.0.csrf,
        active,
        notice,
    }
}

/// Repeated form fields (`players=1&players=2`) arrive as pairs.
fn ids(pairs: &[(String, String)], key: &str) -> Vec<i64> {
    pairs
        .iter()
        .filter(|(k, _)| k == key)
        .filter_map(|(_, v)| v.parse().ok())
        .collect()
}

fn field<'a>(pairs: &'a [(String, String)], key: &str) -> &'a str {
    pairs
        .iter()
        .find(|(k, _)| k == key)
        .map(|(_, v)| v.as_str())
        .unwrap_or("")
}

// ---------------------------------------------------------------- templates

async fn templates(
    State(app): State<App>,
    v: Viewer,
    Query(n): Query<NoticeQ>,
) -> WebResult<Markup> {
    let list = quizzes::templates(&app).await?;
    Ok(page(
        "Templates",
        nav(&v, "templates", notice(&n)),
        html! {
            div.row { h1 style="margin:0" { "Templates" } span.grow {} a.button.primary href="/templates/new" { "New template" } }
            p.muted { "A template is the shape of a quiz night: rounds, and what kind of question goes in each slot." }
            @for t in &list {
                @let shape = Template::parse(&t.body).map(|t| t.shape()).unwrap_or_default();
                div.card.row {
                    a href={"/templates/" (t.id)} { b { (t.name) } }
                    span.muted { (shape.len()) " rounds · " (shape.iter().sum::<usize>()) " questions" }
                }
            }
        },
    ))
}

fn notice(n: &NoticeQ) -> Option<&str> {
    n.notice.as_deref()
}

fn template_form(id: Option<i64>, body: &str, error: Option<&str>) -> Markup {
    html! {
        form.stack method="post" action="/templates" {
            @if let Some(id) = id { input type="hidden" name="id" value=(id); }
            div #tcheck hx-post="/templates/check" hx-trigger="input from:textarea[name=body] delay:500ms" hx-include="[name=body]" hx-target="this" {
                @if let Some(e) = error { p.errors { (e) } }
            }
            label { "Template (TOML)" textarea.code name="body" spellcheck="false" { (body) } }
            div.row {
                button.primary type="submit" { "Save" }
            }
        }
    }
}

async fn new_template(v: Viewer) -> Markup {
    page(
        "New template",
        nav(&v, "templates", None),
        html! { h1 { "New template" } (template_form(None, Template::example(), None)) },
    )
}

async fn edit_template(
    State(app): State<App>,
    v: Viewer,
    Path(id): Path<i64>,
    Query(n): Query<NoticeQ>,
) -> WebResult<Markup> {
    let t = quizzes::template(&app, id).await?;
    Ok(page(
        &t.name,
        nav(&v, "templates", notice(&n)),
        html! {
            div.row {
                h1 style="margin:0" { (t.name) }
                span.grow {}
                form.inline method="post" action={"/templates/" (id) "/delete"} { button.danger type="submit" { "Delete" } }
            }
            (template_form(Some(id), &t.body, None))
            h2 { "Make a quiz from it" }
            (new_quiz_form(&app, Some(id)).await?)
        },
    ))
}

#[derive(Deserialize)]
struct BodyF {
    body: String,
}

async fn check_template(_v: Viewer, axum::extract::Form(f): axum::extract::Form<BodyF>) -> Markup {
    match Template::parse(&f.body) {
        Ok(t) => {
            let shape = t.shape();
            html! { p.small.muted { "✓ " (shape.len()) " rounds, " (shape.iter().sum::<usize>()) " questions: " (shape.iter().map(|n| n.to_string()).collect::<Vec<_>>().join(" + ")) } }
        }
        Err(e) => html! { p.errors { (e.to_string()) } },
    }
}

async fn save_template(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    axum::extract::Form(f): axum::extract::Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let id = field(&f, "id").parse::<i64>().ok();
    let body = field(&f, "body");
    match quizzes::save_template(&app, id, body, v.member(), app.now()).await {
        Ok(id) => Ok(go(
            &headers,
            &with_notice(&format!("/templates/{id}"), "Template saved."),
        )),
        Err(AppError::Invalid(e)) => {
            let html = page(
                "Template",
                nav(&v, "templates", None),
                html! { h1 { "Template" } (template_form(id, body, Some(&e))) },
            );
            Ok(axum::response::IntoResponse::into_response((
                axum::http::StatusCode::UNPROCESSABLE_ENTITY,
                html,
            )))
        }
        Err(e) => Err(e.into()),
    }
}

async fn delete_template(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    quizzes::delete_template(&app, id).await?;
    Ok(go(
        &headers,
        &with_notice("/templates", "Template deleted."),
    ))
}

// ---------------------------------------------------------------- quizzes

async fn new_quiz_form(app: &App, template_id: Option<i64>) -> Result<Markup, AppError> {
    let templates = quizzes::templates(app).await?;
    let players = quizzes::players(app).await?;
    Ok(html! {
        @if templates.is_empty() {
            p.muted { "First " a href="/templates/new" { "make a template" } "." }
        } @else {
            form.stack method="post" action="/quizzes" {
                div.grid2 {
                    label { "Template"
                        select name="template_id" {
                            @for t in &templates { option value=(t.id) selected[Some(t.id) == template_id] { (t.name) } }
                        }
                    }
                    label { "Name" span.hint { "Defaults to the template's name." } input type="text" name="name"; }
                    label { "Seed" span.hint { "Leave empty for a random one. The same seed gives the same quiz." } input type="text" name="seed"; }
                }
                @if !players.is_empty() {
                    fieldset {
                        legend { "Expected players" span.small.muted { " — questions they've already seen are left out" } }
                        div.row { @for p in &players { label.row style="font-weight:400" { input type="checkbox" name="players" value=(p.id); (p.name) } } }
                    }
                }
                div { button.primary type="submit" { "Build quiz" } }
            }
        }
    })
}

async fn quizzes_page(
    State(app): State<App>,
    v: Viewer,
    Query(n): Query<NoticeQ>,
) -> WebResult<Markup> {
    let list = quizzes::quizzes(&app).await?;
    let form = new_quiz_form(&app, None).await?;
    Ok(page(
        "Quizzes",
        nav(&v, "quizzes", notice(&n)),
        html! {
            h1 { "Quizzes" }
            div.card { h3 style="margin-top:0" { "New quiz" } (form) }
            @for q in &list {
                div.card.row {
                    a href={"/quizzes/" (q.id)} { b { (q.name) } }
                    @if q.frozen_at.is_some() { span.badge.accepted { "frozen" } } @else { span.badge.voting { "draft" } }
                    span.muted.small { "seed " code { (q.seed) } }
                }
            }
        },
    ))
}

async fn create_quiz(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    axum::extract::Form(f): axum::extract::Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let tid = field(&f, "template_id")
        .parse::<i64>()
        .map_err(|_| AppError::Invalid("Pick a template.".into()))?;
    let seed = field(&f, "seed");
    let id = quizzes::create_quiz(
        &app,
        tid,
        field(&f, "name"),
        Some(seed),
        &ids(&f, "players"),
        v.member(),
        app.now(),
    )
    .await?;
    Ok(go(&headers, &format!("/quizzes/{id}")))
}

fn slot_row(quiz_id: i64, frozen: bool, s: &SlotView, n: usize) -> Markup {
    let (r, si) = s.pos;
    let base = format!("/quizzes/{quiz_id}/slots/{r}/{si}");
    let classes = format!(
        "card slot{}{}",
        if s.locked { " locked" } else { "" },
        if s.question_id.is_none() {
            " empty"
        } else {
            ""
        }
    );
    html! {
        div class=(classes) id={"slot-" (r) "-" (si)} {
            div.n { (n) }
            div {
                @if let (Some(qid), Some(c)) = (s.question_id, &s.content) {
                    div.row { a href={"/questions/" (qid)} { "#" (qid) } (meta_chips(c)) @if let Some(st) = &s.status { @if st != "accepted" { (status_badge(st)) } } }
                    div { (c.prompt) }
                    (answer_block(c, false))
                    @for w in &s.warnings { p.small.warnbox { (w) } }
                    @if s.newer_available { p.small.warnbox { "A newer version of this question exists." } }
                } @else {
                    p.muted { (s.empty_reason.as_deref().unwrap_or("Empty")) }
                }
                div #{"opts-" (r) "-" (si)} {}
            }
            div.actions.row {
                @if frozen {
                    @if s.newer_available {
                        form.inline method="post" action={(base) "/newer"} { button.small type="submit" { "Use newer" } }
                    }
                } @else {
                    @if s.question_id.is_some() {
                        form.inline method="post" action={(base) (if s.locked { "/unlock" } else { "/lock" })} {
                            button.small type="submit" title="Locked slots survive a refill" { (if s.locked { "🔒 Unlock" } else { "Lock" }) }
                        }
                        form.inline method="post" action={(base) "/clear"} { button.small type="submit" { "Clear" } }
                    }
                    button.small type="button" hx-get={(base) "/options"} hx-target={"#opts-" (r) "-" (si)} { "Swap…" }
                }
            }
        }
    }
}

async fn quiz_page(
    State(app): State<App>,
    v: Viewer,
    Path(id): Path<i64>,
    Query(n): Query<NoticeQ>,
) -> WebResult<Markup> {
    let view = quizzes::view(&app, id).await?;
    let players = quizzes::players(&app).await?;
    let frozen = view.quiz.frozen_at.is_some();
    let total: usize = view.rounds.iter().map(|r| r.slots.len()).sum();
    let filled = view.question_ids().len();
    Ok(page(
        &view.quiz.name,
        nav(&v, "quizzes", notice(&n)),
        html! {
            div.row {
                h1 style="margin:0" { (view.quiz.name) }
                @if frozen { span.badge.accepted { "frozen" } } @else { span.badge.voting { "draft" } }
                span.grow {}
                span.muted { (filled) "/" (total) " filled · seed " code { (view.quiz.seed) } }
            }
            div.row style="margin:12px 0" {
                @if frozen {
                    a.button.primary href={"/quizzes/" (id) "/print/host"} { "Host sheet" }
                    a.button href={"/quizzes/" (id) "/print/sheets"} { "Answer sheets" }
                    a.button href={"/quizzes/" (id) "/print/pictures"} { "Picture handout" }
                    a.button href={"/quizzes/" (id) "/present"} hx-boost="false" { "Present" }
                    a.button href={"/quizzes/" (id) "/answers.csv"} hx-boost="false" { "Answers CSV" }
                    form.inline method="post" action={"/quizzes/" (id) "/unfreeze"} { button type="submit" { "Unfreeze" } }
                } @else {
                    form.inline.row method="post" action={"/quizzes/" (id) "/refill"} {
                        input type="text" name="seed" placeholder="new seed (optional)" style="width:180px";
                        button type="submit" title="Refills every unlocked slot" { "Refill" }
                    }
                    form.inline method="post" action={"/quizzes/" (id) "/freeze"} { button.primary type="submit" { "Freeze for printing" } }
                    form.inline method="post" action={"/quizzes/" (id) "/delete"} { button.danger type="submit" { "Delete" } }
                }
            }
            @if !frozen && !players.is_empty() {
                details {
                    summary { "Expected players (" (view.player_ids.len()) ")" }
                    form.stack method="post" action={"/quizzes/" (id) "/players"} {
                        div.row { @for p in &players { label.row style="font-weight:400" { input type="checkbox" name="players" value=(p.id) checked[view.player_ids.contains(&p.id)]; (p.name) } } }
                        div { button type="submit" { "Save players" } " " span.small.muted { "then Refill to apply" } }
                    }
                }
            }
            @for round in &view.rounds {
                h2 { (round.title) }
                @for s in &round.slots { (slot_row(id, frozen, s, s.pos.1 + 1)) }
            }
            @if frozen {
                h2 { "Schedule an event" }
                (event_form(id, &players))
            }
        },
    ))
}

fn event_form(quiz_id: i64, players: &[quizzes::Player]) -> Markup {
    html! {
        form.stack method="post" action="/events" {
            input type="hidden" name="quiz_id" value=(quiz_id);
            div.grid2 {
                label { "Date" input type="date" name="date" required; }
                label { "Title" input type="text" name="title" placeholder="defaults to the quiz name"; }
            }
            @if !players.is_empty() {
                fieldset { legend { "Players" } div.row { @for p in players { label.row style="font-weight:400" { input type="checkbox" name="players" value=(p.id); (p.name) } } } }
            }
            div { button type="submit" { "Create event" } }
        }
    }
}

async fn slot_options(
    State(app): State<App>,
    _v: Viewer,
    Path((id, r, s)): Path<(i64, usize, usize)>,
) -> WebResult<Markup> {
    let opts = quizzes::swap_options(&app, id, (r, s), 15).await?;
    Ok(html! {
        div.card {
            @if opts.is_empty() { p.muted { "No other question fits this slot." } }
            @for (qid, c) in &opts {
                form.row method="post" action={"/quizzes/" (id) "/slots/" (r) "/" (s) "/set"} style="margin:4px 0" {
                    input type="hidden" name="question_id" value=(qid);
                    button.small type="submit" { "Use" }
                    a href={"/questions/" (qid)} target="_blank" { "#" (qid) }
                    span { (c.prompt) }
                }
            }
        }
    })
}

async fn slot_action(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
    Path((id, r, s, action)): Path<(i64, usize, usize, String)>,
    axum::extract::Form(f): axum::extract::Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let pos = (r, s);
    match action.as_str() {
        "lock" => quizzes::set_locked(&app, id, pos, true).await?,
        "unlock" => quizzes::set_locked(&app, id, pos, false).await?,
        "clear" => quizzes::clear_slot(&app, id, pos).await?,
        "newer" => quizzes::take_newer(&app, id, pos).await?,
        "set" => {
            let qid = field(&f, "question_id")
                .parse::<i64>()
                .map_err(|_| AppError::Invalid("Pick a question.".into()))?;
            quizzes::set_slot(&app, id, pos, qid).await?
        }
        _ => return Err(AppError::NotFound.into()),
    }
    Ok(go(&headers, &format!("/quizzes/{id}#slot-{r}-{s}")))
}

async fn refill(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
    axum::extract::Form(f): axum::extract::Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    quizzes::refill(&app, id, Some(field(&f, "seed")), app.now()).await?;
    Ok(go(
        &headers,
        &with_notice(&format!("/quizzes/{id}"), "Refilled."),
    ))
}

async fn set_players(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
    axum::extract::Form(f): axum::extract::Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    quizzes::set_players(&app, id, &ids(&f, "players")).await?;
    Ok(go(
        &headers,
        &with_notice(&format!("/quizzes/{id}"), "Players saved."),
    ))
}

async fn freeze(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    quizzes::freeze(&app, id, app.now()).await?;
    Ok(go(
        &headers,
        &with_notice(
            &format!("/quizzes/{id}"),
            "Frozen: questions are pinned to their current versions.",
        ),
    ))
}

async fn unfreeze(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    quizzes::unfreeze(&app, id).await?;
    Ok(go(
        &headers,
        &with_notice(&format!("/quizzes/{id}"), "Unfrozen."),
    ))
}

async fn delete_quiz(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    quizzes::delete_quiz(&app, id).await?;
    Ok(go(&headers, &with_notice("/quizzes", "Quiz deleted.")))
}

// ---------------------------------------------------------------- events

async fn events_page(
    State(app): State<App>,
    v: Viewer,
    Query(n): Query<NoticeQ>,
) -> WebResult<Markup> {
    let list = quizzes::events(&app).await?;
    let mut rows = Vec::new();
    for e in &list {
        rows.push((e, quizzes::event_players(&app, e.id).await?));
    }
    Ok(page(
        "Events",
        nav(&v, "events", notice(&n)),
        html! {
            h1 { "Events" }
            p.muted { "Create an event from a frozen quiz. Marking it held records which questions were used and who heard them." }
            @if list.is_empty() { p.muted { "No events yet." } }
            @for (e, players) in &rows {
                div.card.row {
                    b { (e.date) } span { (e.title) }
                    a href={"/quizzes/" (e.quiz_id)} { "quiz" }
                    span.small.muted { (players.iter().map(|p| p.name.as_str()).collect::<Vec<_>>().join(", ")) }
                    span.grow {}
                    @if e.held_at.is_some() {
                        span.badge.accepted { "held" }
                    } @else {
                        form.inline method="post" action={"/events/" (e.id) "/held"} { button.small type="submit" { "Mark as held" } }
                    }
                }
            }
        },
    ))
}

async fn create_event(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
    axum::extract::Form(f): axum::extract::Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let qid = field(&f, "quiz_id")
        .parse::<i64>()
        .map_err(|_| AppError::Invalid("Missing quiz.".into()))?;
    quizzes::create_event(
        &app,
        qid,
        field(&f, "date"),
        field(&f, "title"),
        &ids(&f, "players"),
        app.now(),
    )
    .await?;
    Ok(go(&headers, &with_notice("/events", "Event created.")))
}

async fn mark_held(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    quizzes::mark_held(&app, id, v.member(), app.now()).await?;
    Ok(go(&headers, &with_notice("/events", "Marked as held.")))
}

// ---------------------------------------------------------------- players

async fn players_page(
    State(app): State<App>,
    v: Viewer,
    Query(n): Query<NoticeQ>,
) -> WebResult<Markup> {
    let list = quizzes::players(&app).await?;
    let all = members::list(&app).await?;
    let name_of = |id: i64| {
        all.iter()
            .find(|m| m.id == id)
            .map(|m| m.display_name.clone())
            .unwrap_or_default()
    };
    Ok(page(
        "Players",
        nav(&v, "players", notice(&n)),
        html! {
            h1 { "Players" }
            p.muted { "People who come to quiz nights. Link a player to a group member and the quiz builder knows which questions they've already seen." }
            form.row method="post" action="/players/import" { button type="submit" { "Add every group member as a player" } }
            form.stack method="post" action="/players" style="margin-top:12px" {
                div.grid2 {
                    label { "Name" input type="text" name="name" required; }
                    label { "Group member (optional)"
                        select name="member_id" {
                            option value="" { "—" }
                            @for m in all.iter().filter(|m| !m.is_agent()) { option value=(m.id) { (m.display_name) } }
                        }
                    }
                }
                div { button type="submit" { "Add player" } }
            }
            table style="margin-top:16px" {
                tr { th { "Player" } th { "Member" } }
                @for p in &list { tr { td { (p.name) } td { @if let Some(m) = p.member_id { (name_of(m)) } } } }
            }
        },
    ))
}

async fn add_player(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
    axum::extract::Form(f): axum::extract::Form<Vec<(String, String)>>,
) -> WebResult<Response> {
    let member = field(&f, "member_id").parse::<i64>().ok();
    quizzes::add_player(&app, field(&f, "name"), member, app.now()).await?;
    Ok(go(&headers, &with_notice("/players", "Player added.")))
}

async fn import_players(
    State(app): State<App>,
    _v: Viewer,
    headers: HeaderMap,
) -> WebResult<Response> {
    let n = quizzes::import_members_as_players(&app, app.now()).await?;
    Ok(go(
        &headers,
        &with_notice("/players", &format!("{n} player(s) added.")),
    ))
}
