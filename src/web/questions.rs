//! Question pages: dashboard, bank, submit and edit forms, detail with history,
//! votes and proposals.

use super::layout::{self, Nav, changes_table, content_view, page, status_badge};
use super::{Viewer, WebError, WebResult, go, with_notice};
use crate::domain::dedupe::{Band, Match};
use crate::domain::model::{Content, Kind, MediaKind, MediaRef, Patch, format_parts, parse_parts};
use crate::domain::rules::RuleReport;
use crate::domain::voting::Vote;
use crate::service::questions::{self, DecideOutcome, EditOutcome, Filter, SubmitOutcome};
use crate::service::{App, AppError, media, members};
use axum::Router;
use axum::extract::{Form, Multipart, Path, Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use maud::{Markup, html};
use serde::Deserialize;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/questions", get(list).post(create))
        .route("/questions/new", get(new_form))
        .route("/questions/check", post(check))
        .route("/questions/{id}", get(detail))
        .route("/questions/{id}/edit", get(edit_form).post(edit))
        .route("/questions/{id}/vote", post(vote))
        .route("/questions/{id}/resubmit", post(resubmit))
        .route("/questions/{id}/retire", post(retire))
        .route("/questions/{id}/notes", post(add_note))
        .route("/proposals/{id}/decide", post(decide))
        .route("/proposals/{id}/withdraw", post(withdraw))
        .route("/themes", post(add_theme))
        .route("/media/{id}", get(serve_media))
}

#[derive(Deserialize, Default)]
pub struct NoticeQ {
    pub notice: Option<String>,
}

fn nav<'a>(v: &'a Viewer, active: &'a str, notice: Option<&'a str>) -> Nav<'a> {
    Nav {
        me: v.member(),
        csrf: &v.0.csrf,
        active,
        notice,
    }
}

// ---------------------------------------------------------------- dashboard

pub async fn dashboard(
    State(app): State<App>,
    v: Viewer,
    Query(n): Query<NoticeQ>,
) -> WebResult<Markup> {
    let me = v.member();
    let to_vote = questions::list(
        &app,
        &Filter {
            needs_vote_by: Some(me.id),
            limit: 10,
            ..Default::default()
        },
    )
    .await?;
    let inbox = questions::inbox(&app, me).await?;
    let mine = questions::list(
        &app,
        &Filter {
            author_id: Some(me.id),
            limit: 5,
            ..Default::default()
        },
    )
    .await?;
    let stats = questions::stats(&app).await?;
    let flagged = questions::list(
        &app,
        &Filter {
            flagged: true,
            limit: 5,
            ..Default::default()
        },
    )
    .await?;
    let count = |s: &str| {
        stats
            .by_status
            .iter()
            .find(|(k, _)| k == s)
            .map(|(_, n)| *n)
            .unwrap_or(0)
    };
    Ok(page(
        "Home",
        nav(&v, "", n.notice.as_deref()),
        html! {
            h1 { "Hi, " (me.display_name) }
            div.grid2 {
                div.card { div.stat { (count("accepted")) } div.muted { "accepted questions" } }
                div.card { div.stat { (count("voting")) } div.muted { "waiting for votes" } }
            }
            h2 { "Waiting for your vote" }
            @if to_vote.is_empty() { p.muted { "Nothing to vote on. " a href="/questions/new" { "Submit a question?" } } }
            div.qlist { @for s in &to_vote { (summary_card(s)) } }
            @if !inbox.is_empty() {
                h2 { "Proposals for you to decide" }
                @for (p, prompt) in &inbox {
                    div.card {
                        a href={"/questions/" (p.question_id) "#proposal-" (p.id)} { "#" (p.question_id) " · " (prompt) }
                        @if p.needs_second_approval { span.chip { "needs a second approval" } }
                    }
                }
            }
            @if !flagged.is_empty() {
                h2 { "Flagged by the review agent" }
                div.qlist { @for s in &flagged { (summary_card(s)) } }
            }
            h2 { "Your latest" }
            div.qlist { @for s in &mine { (summary_card(s)) } }
            @if !stats.by_theme.is_empty() {
                h2 { "Accepted per theme" }
                p { @for (t, n) in &stats.by_theme { span.chip { (t) " " (n) } " " } }
            }
        },
    ))
}

fn summary_card(s: &questions::Summary) -> Markup {
    html! {
        div.card {
            div.row {
                a href={"/questions/" (s.id)} { "#" (s.id) }
                (status_badge(&s.status))
                @if let Some(f) = &s.review_flag { (status_badge(f)) }
                span.chip { (questions::kind_of(&s.kind).label()) }
                span.chip { (s.theme) }
                span.chip { (layout::stars(s.difficulty)) }
                @if s.open_proposals > 0 { span.chip { (s.open_proposals) " proposal(s)" } }
                span.grow {}
                span.small.muted { "by " (s.author_name) }
            }
            a.prompt href={"/questions/" (s.id)} style="text-decoration:none;color:inherit" { (s.prompt) }
            details.answer { summary { "Answer" } b { (s.answer) } }
        }
    }
}

// ---------------------------------------------------------------- bank

#[derive(Deserialize, Default)]
pub struct ListQ {
    status: Option<String>,
    theme: Option<String>,
    kind: Option<String>,
    difficulty: Option<String>,
    language: Option<String>,
    q: Option<String>,
    mine: Option<String>,
    needs_vote: Option<String>,
    flagged: Option<String>,
    page: Option<i64>,
    notice: Option<String>,
}

fn nonempty(s: &Option<String>) -> Option<String> {
    s.as_ref()
        .map(|x| x.trim().to_string())
        .filter(|x| !x.is_empty())
}

const PAGE: i64 = 50;

async fn list(State(app): State<App>, v: Viewer, Query(q): Query<ListQ>) -> WebResult<Markup> {
    let me = v.member().id;
    let page_n = q.page.unwrap_or(0).max(0);
    let f = Filter {
        status: nonempty(&q.status),
        theme: nonempty(&q.theme),
        kind: nonempty(&q.kind),
        difficulty: nonempty(&q.difficulty).and_then(|d| d.parse().ok()),
        language: nonempty(&q.language),
        text: nonempty(&q.q),
        author_id: q.mine.is_some().then_some(me),
        needs_vote_by: q.needs_vote.is_some().then_some(me),
        flagged: q.flagged.is_some(),
        limit: PAGE + 1,
        offset: page_n * PAGE,
    };
    let mut rows = questions::list(&app, &f).await?;
    let more = rows.len() as i64 > PAGE;
    rows.truncate(PAGE as usize);
    let themes = questions::themes(&app).await?;
    let sel = |cur: &Option<String>, val: &str| cur.as_deref() == Some(val);
    let active = if q.needs_vote.is_some() {
        "vote"
    } else {
        "bank"
    };
    let mut next_q = url::form_urlencoded::Serializer::new(String::new());
    for (k, val) in [
        ("status", &q.status),
        ("theme", &q.theme),
        ("kind", &q.kind),
        ("difficulty", &q.difficulty),
        ("language", &q.language),
        ("q", &q.q),
    ] {
        if let Some(x) = nonempty(val) {
            next_q.append_pair(k, &x);
        }
    }
    for (k, val) in [
        ("mine", &q.mine),
        ("needs_vote", &q.needs_vote),
        ("flagged", &q.flagged),
    ] {
        if val.is_some() {
            next_q.append_pair(k, "1");
        }
    }
    let base_q = next_q.finish();
    Ok(page(
        "Bank",
        nav(&v, active, q.notice.as_deref()),
        html! {
            h1 { @if q.needs_vote.is_some() { "Waiting for your vote" } @else { "Question bank" } }
            form.filters method="get" action="/questions" hx-get="/questions" hx-trigger="change, keyup changed delay:400ms from:find input[name=q]" hx-target="#results" hx-select="#results" hx-push-url="true" {
                input type="search" name="q" placeholder="Search" value=[q.q.as_deref()];
                select name="status" {
                    option value="" { "Any status" }
                    @for s in ["voting", "accepted", "rejected", "retired"] { option value=(s) selected[sel(&q.status, s)] { (s) } }
                }
                select name="theme" {
                    option value="" { "Any theme" }
                    @for t in &themes { option value=(t) selected[sel(&q.theme, t)] { (t) } }
                }
                select name="kind" {
                    option value="" { "Any kind" }
                    @for k in Kind::ALL { option value=(k.as_str()) selected[sel(&q.kind, k.as_str())] { (k.label()) } }
                }
                select name="difficulty" {
                    option value="" { "Any difficulty" }
                    @for d in 1..=5 { option value=(d) selected[sel(&q.difficulty, &d.to_string())] { (layout::stars(d)) } }
                }
                input type="text" name="language" placeholder="Language" value=[q.language.as_deref()];
                label.row style="font-weight:400" { input type="checkbox" name="mine" value="1" checked[q.mine.is_some()]; "Mine" }
                label.row style="font-weight:400" { input type="checkbox" name="flagged" value="1" checked[q.flagged.is_some()]; "Flagged" }
                @if q.needs_vote.is_some() { input type="hidden" name="needs_vote" value="1"; }
            }
            div #results {
                @if rows.is_empty() { p.muted { "No questions match." } }
                div.qlist { @for s in &rows { (summary_card(s)) } }
                div.row {
                    @if page_n > 0 { a.button href={"/questions?" (base_q) "&page=" (page_n - 1)} { "← Newer" } }
                    @if more { a.button href={"/questions?" (base_q) "&page=" (page_n + 1)} { "Older →" } }
                }
            }
        },
    ))
}

// ---------------------------------------------------------------- forms

#[derive(Debug, Default)]
struct QForm {
    kind: String,
    prompt: String,
    answer: String,
    accepted: String,
    choices: String,
    parts: String,
    points: String,
    difficulty: String,
    theme: String,
    new_theme: String,
    tags: String,
    language: String,
    background: String,
    fun_fact: String,
    host_notes: String,
    sources: String,
    media: Vec<String>,
    confirm: bool,
    rationale: String,
    files: Vec<Vec<u8>>,
}

async fn read_form(mut mp: Multipart) -> Result<QForm, WebError> {
    let bad = |e: axum::extract::multipart::MultipartError| {
        WebError(AppError::Invalid(format!("Couldn't read the form: {e}")))
    };
    let mut f = QForm::default();
    while let Some(field) = mp.next_field().await.map_err(bad)? {
        let name = field.name().unwrap_or("").to_string();
        if name == "files" {
            let bytes = field.bytes().await.map_err(bad)?;
            if !bytes.is_empty() {
                f.files.push(bytes.to_vec());
            }
            continue;
        }
        let val = field.text().await.map_err(bad)?;
        match name.as_str() {
            "kind" => f.kind = val,
            "prompt" => f.prompt = val,
            "answer" => f.answer = val,
            "accepted" => f.accepted = val,
            "choices" => f.choices = val,
            "parts" => f.parts = val,
            "points" => f.points = val,
            "difficulty" => f.difficulty = val,
            "theme" => f.theme = val,
            "new_theme" => f.new_theme = val,
            "tags" => f.tags = val,
            "language" => f.language = val,
            "background" => f.background = val,
            "fun_fact" => f.fun_fact = val,
            "host_notes" => f.host_notes = val,
            "sources" => f.sources = val,
            "media" => f.media.push(val),
            "confirm" => f.confirm = val == "1",
            "rationale" => f.rationale = val,
            _ => {}
        }
    }
    Ok(f)
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

fn media_refs(values: &[String]) -> Vec<MediaRef> {
    values
        .iter()
        .filter_map(|v| {
            let (id, kind) = v.split_once(':')?;
            Some(MediaRef {
                media_id: id.parse().ok()?,
                kind: if kind == "audio" {
                    MediaKind::Audio
                } else {
                    MediaKind::Image
                },
                start_s: None,
                end_s: None,
            })
        })
        .collect()
}

impl QForm {
    fn content(&self, default_language: &str) -> Content {
        let kind: Kind = self.kind.parse().unwrap_or(Kind::Text);
        let theme = if self.new_theme.trim().is_empty() {
            self.theme.clone()
        } else {
            questions::normalize_theme(&self.new_theme)
        };
        Content {
            kind,
            prompt: self.prompt.clone(),
            answer: self.answer.clone(),
            accepted_answers: lines(&self.accepted),
            choices: if kind == Kind::MultipleChoice {
                lines(&self.choices)
            } else {
                vec![]
            },
            parts: if kind == Kind::MultiPart {
                parse_parts(&self.parts)
            } else {
                vec![]
            },
            points: self.points.trim().parse().unwrap_or(1),
            media: media_refs(&self.media),
            difficulty: self.difficulty.trim().parse().unwrap_or(3),
            theme,
            tags: self.tags.split([',', ' ']).map(String::from).collect(),
            language: if self.language.trim().is_empty() {
                default_language.to_string()
            } else {
                self.language.clone()
            },
            background: self.background.clone(),
            fun_fact: self.fun_fact.clone(),
            host_notes: self.host_notes.clone(),
            sources: lines(&self.sources),
        }
        .normalized()
    }
}

/// Stores uploaded files and adds them to the content.
async fn attach_uploads(
    app: &App,
    f: &QForm,
    by: i64,
    mut c: Content,
) -> Result<Content, WebError> {
    for bytes in &f.files {
        let m = media::store(app, bytes, by, app.now()).await?;
        if !c.media.iter().any(|x| x.media_id == m.id) {
            c.media.push(MediaRef {
                media_id: m.id,
                kind: m.media_kind(),
                start_s: None,
                end_s: None,
            });
        }
    }
    Ok(c)
}

async fn ensure_theme(app: &App, f: &QForm, by: i64) -> Result<(), WebError> {
    if !f.new_theme.trim().is_empty() {
        questions::add_theme(app, &f.new_theme, Some(by), app.now()).await?;
    }
    Ok(())
}

fn feedback(report: Option<&RuleReport>, dups: &[Match]) -> Markup {
    html! {
        @if let Some(r) = report {
            @if !r.errors.is_empty() {
                div.errors { ul { @for e in &r.errors { li { (e.message()) } } } }
            }
            @if r.reactions.contains(&crate::domain::rules::Reaction::Boo) && r.errors.is_empty() {
                div.warnbox { "A number answer. Allowed here, but the group will hear a “Boo!”." }
            }
        }
        @if !dups.is_empty() {
            div.warnbox {
                "Similar questions already exist:"
                ul {
                    @for m in dups {
                        li {
                            a href={"/questions/" (m.question_id)} target="_blank" { "#" (m.question_id) }
                            " · " (m.band.label()) " (" (m.status) ", " (format!("{:.0} %", m.score * 100.0)) ")"
                            @if m.band == Band::LikelyDuplicate { " ⚠" }
                        }
                    }
                }
            }
        }
    }
}

fn question_form(
    action: &str,
    c: &Content,
    themes: &[String],
    extra: Markup,
    feedback_html: Markup,
    editing: bool,
) -> Markup {
    let kind_is = |k: Kind| c.kind == k;
    html! {
        form #qform.stack method="post" action=(action) enctype="multipart/form-data" {
            div #feedback hx-post="/questions/check" hx-trigger="input from:#qform delay:700ms, change from:#qform" hx-include="#qform" hx-encoding="multipart/form-data" hx-target="this" {
                (feedback_html)
            }
            div.grid2 {
                label { "Kind"
                    select name="kind" {
                        @for k in Kind::ALL { option value=(k.as_str()) selected[kind_is(k)] { (k.label()) } }
                    }
                }
                label { "Difficulty"
                    select name="difficulty" {
                        @for d in 1..=5u8 { option value=(d) selected[c.difficulty == d] { (d) " " (layout::stars(i64::from(d))) } }
                    }
                }
            }
            label { "Question" textarea name="prompt" required { (c.prompt) } }
            label { "Answer" span.hint { "For multi-part questions this can stay empty." } input type="text" name="answer" value=(c.answer); }
            label { "Also accepted" span.hint { "One per line: spellings, short forms." } textarea name="accepted" { (c.accepted_answers.join("\n")) } }
            label { "Choices" span.hint { "Multiple choice only: 3–6 lines, one of them the answer." } textarea name="choices" { (c.choices.join("\n")) } }
            label { "Parts" span.hint { "Multi-part only, one per line: " code { "[prompt ::] answer [; points]" } } textarea name="parts" { (format_parts(&c.parts)) } }
            div.grid2 {
                label { "Theme"
                    select name="theme" {
                        @for t in themes { option value=(t) selected[&c.theme == t] { (t) } }
                    }
                }
                label { "…or a new theme" input type="text" name="new_theme" placeholder="e.g. board games"; }
                label { "Language" input type="text" name="language" value=(c.language); }
                label { "Points" input type="number" name="points" min="1" value=(c.points); }
            }
            label { "Tags" span.hint { "Comma-separated." } input type="text" name="tags" value=(c.tags.join(", ")); }
            @if !c.media.is_empty() {
                fieldset {
                    legend { "Attached media (untick to remove)" }
                    @for m in &c.media {
                        @let val = format!("{}:{}", m.media_id, if m.kind == MediaKind::Audio { "audio" } else { "image" });
                        label.row style="font-weight:400" {
                            input type="checkbox" name="media" value=(val) checked;
                            @if m.kind == MediaKind::Image { img src={"/media/" (m.media_id)} alt="" style="max-height:80px"; } @else { "Audio #" (m.media_id) }
                        }
                    }
                }
            }
            label { "Add images or audio" span.hint { "Image and audio questions only. JPEG/PNG/GIF/WebP, MP3/OGG/M4A/FLAC/WAV." } input type="file" name="files" multiple accept="image/*,audio/*"; }
            label { "Fun fact" span.hint { "Read out after the answer." } textarea name="fun_fact" { (c.fun_fact) } }
            label { "Background" span.hint { "The longer story, for the host." } textarea name="background" { (c.background) } }
            label { "Host notes" span.hint { "Pronunciation, what to accept." } textarea name="host_notes" { (c.host_notes) } }
            label { "Sources" span.hint { "One URL per line." } textarea name="sources" { (c.sources.join("\n")) } }
            @if editing { label { "Why this change?" input type="text" name="rationale"; } }
            (extra)
        }
    }
}

async fn new_form(State(app): State<App>, v: Viewer) -> WebResult<Markup> {
    let themes = questions::themes(&app).await?;
    let mut c = Content::text(
        "",
        "",
        themes.first().map(String::as_str).unwrap_or(""),
        &app.cfg.default_language,
    );
    c.points = 1;
    Ok(page(
        "Submit",
        nav(&v, "new", None),
        html! {
            h1 { "Submit a question" }
            p.muted { "No yes/no questions. Number answers belong in tiebreakers (years are fine). The group votes on every question." }
            (question_form("/questions", &c, &themes, html! { button.primary type="submit" { "Submit for voting" } }, html! {}, false))
        },
    ))
}

async fn check(State(app): State<App>, v: Viewer, mp: Multipart) -> WebResult<Markup> {
    let _ = v;
    let f = read_form(mp).await?;
    let c = f.content(&app.cfg.default_language);
    if c.prompt.is_empty() {
        return Ok(html! {});
    }
    let (_, report, dups) = questions::check_draft(&app, c, None).await?;
    Ok(feedback(Some(&report), &dups))
}

async fn create(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    mp: Multipart,
) -> WebResult<Response> {
    let me = v.member().clone();
    members::require_in_group(&app, &me, app.now()).await?;
    let f = read_form(mp).await?;
    ensure_theme(&app, &f, me.id).await?;
    let c = attach_uploads(&app, &f, me.id, f.content(&app.cfg.default_language)).await?;
    let themes = questions::themes(&app).await?;
    let again = |fb: Markup, confirm: bool| {
        let extra = html! {
            @if confirm {
                label.row style="font-weight:400" { input type="checkbox" name="confirm" value="1"; "It's not a duplicate — submit anyway" }
            }
            button.primary type="submit" { "Submit for voting" }
        };
        page(
            "Submit",
            nav(&v, "new", None),
            html! { h1 { "Submit a question" } (question_form("/questions", &c, &themes, extra, fb, false)) },
        )
    };
    match questions::submit(&app, &me, c.clone(), f.confirm, app.now()).await? {
        SubmitOutcome::Submitted(id) => Ok(go(
            &headers,
            &with_notice(
                &format!("/questions/{id}"),
                "Submitted. The group can vote now.",
            ),
        )),
        SubmitOutcome::Refused(r) => Ok((
            StatusCode::UNPROCESSABLE_ENTITY,
            again(feedback(Some(&r), &[]), false),
        )
            .into_response()),
        SubmitOutcome::Duplicates(d) => {
            Ok((StatusCode::OK, again(feedback(None, &d), true)).into_response())
        }
    }
}

// ---------------------------------------------------------------- detail

async fn detail(
    State(app): State<App>,
    v: Viewer,
    Path(id): Path<i64>,
    Query(n): Query<NoticeQ>,
) -> WebResult<Markup> {
    let d = questions::detail(&app, id).await?;
    let me = v.member();
    let is_author = me.id == d.q.author_id;
    let round = d.rounds.first();
    let my_vote = round.and_then(|r| {
        r.votes
            .iter()
            .find(|(m, _, _)| *m == me.id)
            .map(|(_, _, v)| *v)
    });
    let open_round = round.filter(|r| r.row.closed_at.is_none() && d.q.status == "voting");
    let orphan = !d.author.in_group;
    Ok(page(
        &format!("Question #{id}"),
        nav(&v, "bank", n.notice.as_deref()),
        html! {
            div.row {
                h1 style="margin:0" { "Question #" (id) }
                (status_badge(&d.q.status))
                @if let Some(f) = &d.q.review_flag { (status_badge(f)) }
                span.grow {}
                span.muted { "by " (d.author.display_name) " · version " (d.current.row.n) }
            }
            div.card { (content_view(&d.current.content, false)) }
            @if let Some(r) = d.current.row.report() {
                @if r.reactions.contains(&crate::domain::rules::Reaction::Boo) { p.small.muted { "🍅 Number answer: this one got a Boo." } }
            }
            @let dups = d.current.row.duplicates();
            @if !dups.is_empty() { (feedback(None, &dups)) }

            div.row {
                a.button href={"/questions/" (id) "/edit"} { @if is_author { "Edit" } @else { "Propose a change" } }
                @if is_author && d.q.status == "rejected" {
                    form.inline method="post" action={"/questions/" (id) "/resubmit"} { button type="submit" { "Resubmit unchanged" } }
                }
                @if d.q.status == "accepted" {
                    form.inline method="post" action={"/questions/" (id) "/retire"} { input type="hidden" name="retired" value="true"; button.danger type="submit" { "Retire" } }
                }
                @if d.q.status == "retired" {
                    form.inline method="post" action={"/questions/" (id) "/retire"} { input type="hidden" name="retired" value="false"; button type="submit" { "Bring back" } }
                }
            }

            h2 { "Voting" }
            @if let Some(r) = round {
                div.card {
                    p {
                        "Round " (r.row.round) " · "
                        "✅ " (r.votes.iter().filter(|(_, _, v)| *v == Vote::Yes).count())
                        " · ❌ " (r.votes.iter().filter(|(_, _, v)| *v == Vote::No).count())
                        @if let Some(o) = &r.row.outcome { " · " (o) } @else { " · closes " (short_ts(&r.row.closes_at)) }
                    }
                    @if !r.votes.is_empty() {
                        p.small.muted { @for (i, (_, name, vote)) in r.votes.iter().enumerate() { @if i > 0 { ", " } (name) " " (if *vote == Vote::Yes { "✅" } else { "❌" }) } }
                    }
                    @if open_round.is_some() && !is_author {
                        div.row {
                            form.inline method="post" action={"/questions/" (id) "/vote"} {
                                input type="hidden" name="round" value=(r.row.round);
                                input type="hidden" name="value" value="yes";
                                button.primary type="submit" disabled[my_vote == Some(Vote::Yes)] { "✅ Add it" }
                            }
                            form.inline method="post" action={"/questions/" (id) "/vote"} {
                                input type="hidden" name="round" value=(r.row.round);
                                input type="hidden" name="value" value="no";
                                button type="submit" disabled[my_vote == Some(Vote::No)] { "❌ No" }
                            }
                            @if let Some(mv) = my_vote { span.muted { "You voted " (mv.as_str()) "; you can change it." } }
                        }
                    }
                }
            }

            @if !d.proposals.is_empty() {
                h2 { "Proposals" }
                @for p in &d.proposals {
                    div.card id={"proposal-" (p.row.id)} {
                        div.row {
                            b { "#" (p.row.id) }
                            (status_badge(&p.row.status))
                            span { "by " (p.proposer_name) " on version " (p.base_n) }
                            @if p.row.needs_second_approval { span.chip { "needs a second approval" } }
                            @if p.row.material { span.chip { "material" } }
                            @if let Some(by) = &p.decided_by_name { span.small.muted { "decided by " (by) } }
                        }
                        @if !p.row.rationale.is_empty() { p.small { i { (p.row.rationale) } } }
                        (changes_table(&p.changes))
                        @if p.row.status == "open" {
                            @let can_decide = p.row.proposer_id != me.id && (p.row.needs_second_approval || is_author || orphan);
                            div.row style="margin-top:8px" {
                                @if can_decide {
                                    form.inline method="post" action={"/proposals/" (p.row.id) "/decide"} { input type="hidden" name="accept" value="true"; button.primary type="submit" { "Accept" } }
                                    form.inline method="post" action={"/proposals/" (p.row.id) "/decide"} { input type="hidden" name="accept" value="false"; button type="submit" { "Decline" } }
                                }
                                @if p.row.proposer_id == me.id {
                                    form.inline method="post" action={"/proposals/" (p.row.id) "/withdraw"} { button type="submit" { "Withdraw" } }
                                }
                            }
                        }
                    }
                }
            }

            @if !d.reviews.is_empty() {
                h2 { "AI reviews" }
                @for r in &d.reviews {
                    div.card {
                        div.row { (status_badge(&r.verdict)) span.small.muted { (short_ts(&r.created_at)) } }
                        p { (r.explanation) }
                        @let sources: Vec<String> = serde_json::from_str(&r.sources).unwrap_or_default();
                        @if !sources.is_empty() { p.small { @for s in &sources { a href=(s) rel="noopener noreferrer" target="_blank" { (s) } " " } } }
                        @let flags: Vec<String> = serde_json::from_str(&r.flags).unwrap_or_default();
                        @if !flags.is_empty() { p { @for fl in &flags { span.chip { (fl) } " " } } }
                        @if r.auto_applied_version_id.is_some() { p.small.muted { "Typo fixes were applied automatically." } }
                        @if let Some(t) = &r.typo_rejected { p.small.muted { "Suggested typo fixes became a proposal: " (t) } }
                    }
                }
            }

            h2 { "Notes" }
            @for note in &d.notes {
                div.card {
                    p { (note.body) }
                    p.small.muted {
                        (note.author_name) " · " (short_ts(&note.created_at))
                        @if let Some(l) = note.links_question_id { @if l != id { " · see " a href={"/questions/" (l)} { "#" (l) } } }
                    }
                }
            }
            form.stack method="post" action={"/questions/" (id) "/notes"} {
                textarea name="body" placeholder="Add a note for the group" {}
                div { button type="submit" { "Add note" } }
            }

            @if !d.used_at.is_empty() {
                h2 { "Used at" }
                ul { @for (_, title, date) in &d.used_at { li { (date) " · " (title) } } }
            }

            h2 { "History" }
            @for (i, ver) in d.versions.iter().enumerate() {
                div.card {
                    div.row {
                        b { "Version " (ver.row.n) }
                        span.muted { (ver.by_name) " · " (short_ts(&ver.row.created_at)) }
                        span { (ver.row.reason) }
                    }
                    @if let Some(prev) = d.versions.get(i + 1) {
                        (changes_table(&Patch::between(&prev.content, &ver.content).describe(&prev.content)))
                    }
                }
            }
        },
    ))
}

pub fn short_ts(s: &str) -> String {
    crate::db::parse_ts(s)
        .format("%Y-%m-%d %H:%M UTC")
        .to_string()
}

// ---------------------------------------------------------------- edit

async fn edit_form(State(app): State<App>, v: Viewer, Path(id): Path<i64>) -> WebResult<Markup> {
    let d = questions::detail(&app, id).await?;
    let themes = questions::themes(&app).await?;
    let is_author = v.member().id == d.q.author_id;
    let what = if is_author {
        "Edit"
    } else {
        "Propose a change to"
    };
    Ok(page(
        "Edit",
        nav(&v, "bank", None),
        html! {
            h1 { (what) " question #" (id) }
            @if !is_author { p.muted { "The author decides whether your change goes in." } }
            @if is_author && d.q.status == "voting" { p.muted { "Changing the question, answer, choices, parts or media restarts the vote." } }
            @if is_author && d.q.status == "accepted" { p.muted { "Changing the question, answer, choices, parts or media of an accepted question needs another member's approval." } }
            (question_form(&format!("/questions/{id}/edit"), &d.current.content, &themes, html! { button.primary type="submit" { "Save" } }, html! {}, true))
        },
    ))
}

async fn edit(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
    mp: Multipart,
) -> WebResult<Response> {
    let me = v.member().clone();
    members::require_in_group(&app, &me, app.now()).await?;
    let f = read_form(mp).await?;
    ensure_theme(&app, &f, me.id).await?;
    let c = attach_uploads(&app, &f, me.id, f.content(&app.cfg.default_language)).await?;
    let notice = match questions::edit(&app, id, &me, c, &f.rationale, app.now()).await? {
        EditOutcome::NoChange => "Nothing changed.".to_string(),
        EditOutcome::Applied { reopened: true, .. } => "Saved. The vote starts over.".to_string(),
        EditOutcome::Applied { .. } => "Saved.".to_string(),
        EditOutcome::Proposed(pid) => format!("Proposal #{pid} filed."),
    };
    Ok(go(
        &headers,
        &with_notice(&format!("/questions/{id}"), &notice),
    ))
}

// ---------------------------------------------------------------- actions

#[derive(Deserialize)]
struct VoteF {
    value: String,
    round: Option<i64>,
}

async fn vote(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<VoteF>,
) -> WebResult<Response> {
    let me = v.member().clone();
    members::require_in_group(&app, &me, app.now()).await?;
    let value =
        Vote::parse(&f.value).ok_or_else(|| AppError::Invalid("A vote is yes or no.".into()))?;
    let out = questions::vote(&app, id, f.round, &me, value, app.now()).await?;
    let notice = match out.outcome {
        Some(_) => "Your vote decided it: accepted.".to_string(),
        None => format!("Vote recorded (✅ {} · ❌ {}).", out.yes, out.no),
    };
    Ok(go(
        &headers,
        &with_notice(&format!("/questions/{id}"), &notice),
    ))
}

async fn resubmit(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    questions::resubmit(&app, id, v.member(), app.now()).await?;
    Ok(go(
        &headers,
        &with_notice(&format!("/questions/{id}"), "Resubmitted for voting."),
    ))
}

#[derive(Deserialize)]
struct RetireF {
    retired: bool,
}

async fn retire(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<RetireF>,
) -> WebResult<Response> {
    questions::set_retired(&app, id, v.member(), f.retired, app.now()).await?;
    Ok(go(
        &headers,
        &with_notice(
            &format!("/questions/{id}"),
            if f.retired {
                "Retired."
            } else {
                "Back in use."
            },
        ),
    ))
}

#[derive(Deserialize)]
struct NoteF {
    body: String,
}

async fn add_note(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    Path(id): Path<i64>,
    Form(f): Form<NoteF>,
) -> WebResult<Response> {
    questions::add_note(&app, id, v.member(), &f.body, None, app.now()).await?;
    Ok(go(&headers, &format!("/questions/{id}")))
}

#[derive(Deserialize)]
struct DecideF {
    accept: bool,
}

async fn decide(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    Path(pid): Path<i64>,
    Form(f): Form<DecideF>,
) -> WebResult<Response> {
    let me = v.member().clone();
    members::require_in_group(&app, &me, app.now()).await?;
    let qid = {
        let mut conn = app.db.acquire().await.map_err(AppError::from)?;
        questions::proposal(&mut conn, pid).await?.question_id
    };
    let notice = match questions::decide(&app, pid, &me, f.accept, app.now()).await? {
        DecideOutcome::Accepted { .. } => "Proposal accepted.".to_string(),
        DecideOutcome::Declined => "Proposal declined.".to_string(),
        DecideOutcome::Stale(fields) => format!(
            "That proposal is out of date ({} changed since); the proposer was told.",
            fields
                .iter()
                .map(|f| f.label().to_lowercase())
                .collect::<Vec<_>>()
                .join(", ")
        ),
    };
    Ok(go(
        &headers,
        &with_notice(&format!("/questions/{qid}"), &notice),
    ))
}

async fn withdraw(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    Path(pid): Path<i64>,
) -> WebResult<Response> {
    let qid = {
        let mut conn = app.db.acquire().await.map_err(AppError::from)?;
        questions::proposal(&mut conn, pid).await?.question_id
    };
    questions::withdraw(&app, pid, v.member(), app.now()).await?;
    Ok(go(
        &headers,
        &with_notice(&format!("/questions/{qid}"), "Proposal withdrawn."),
    ))
}

#[derive(Deserialize)]
struct ThemeF {
    name: String,
}

async fn add_theme(
    State(app): State<App>,
    v: Viewer,
    headers: HeaderMap,
    Form(f): Form<ThemeF>,
) -> WebResult<Response> {
    let name = questions::add_theme(&app, &f.name, Some(v.member().id), app.now()).await?;
    Ok(go(
        &headers,
        &with_notice("/questions", &format!("Theme “{name}” added.")),
    ))
}

async fn serve_media(
    State(app): State<App>,
    _v: Viewer,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    let m = media::get(&app, id).await?;
    let bytes = tokio::fs::read(media::path_for(&app, &m.sha256))
        .await
        .map_err(|_| AppError::NotFound)?;
    Ok((
        [
            (header::CONTENT_TYPE, m.mime),
            (header::CACHE_CONTROL, "private, max-age=86400".to_string()),
        ],
        bytes,
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::Part;

    #[test]
    fn parts_round_trip() {
        let parts = parse_parts("North :: Oulu ; 2\nTurku\n :: Pori\nVaasa ; x");
        assert_eq!(
            parts[0],
            Part {
                prompt: Some("North".into()),
                answer: "Oulu".into(),
                points: 2
            }
        );
        assert_eq!(
            parts[1],
            Part {
                prompt: None,
                answer: "Turku".into(),
                points: 1
            }
        );
        assert_eq!(
            parts[2],
            Part {
                prompt: None,
                answer: "Pori".into(),
                points: 1
            }
        );
        assert_eq!(parts[3].answer, "Vaasa ; x");
        assert_eq!(format_parts(&parts[..2]), "North :: Oulu ; 2\nTurku");
        let refs = media_refs(&[
            "3:image".into(),
            "4:audio".into(),
            "x".into(),
            "y:image".into(),
        ]);
        assert_eq!(refs.len(), 2);
        assert_eq!(refs[1].kind, MediaKind::Audio);
    }

    #[test]
    fn form_to_content() {
        let f = QForm {
            kind: "multiple_choice".into(),
            prompt: " Largest lake? ".into(),
            answer: "Saimaa".into(),
            choices: "Saimaa\nPäijänne\n\nInari".into(),
            parts: "ignored".into(),
            difficulty: "2".into(),
            theme: "geography".into(),
            tags: "lakes, finland lakes".into(),
            ..Default::default()
        };
        let c = f.content("fi");
        assert_eq!(c.kind, Kind::MultipleChoice);
        assert_eq!(c.choices.len(), 3);
        assert!(c.parts.is_empty());
        assert_eq!(c.language, "fi");
        assert_eq!(c.tags, vec!["lakes", "finland"]);
        assert_eq!(c.difficulty, 2);
        let g = QForm {
            new_theme: "  Board  Games ".into(),
            ..Default::default()
        };
        assert_eq!(g.content("en").theme, "board games");
    }
}
