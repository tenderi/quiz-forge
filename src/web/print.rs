//! Printouts for a paper quiz night (DESIGN.md §10), a presenter view and the
//! answer key as CSV. All read the frozen quiz.

use super::layout::stars;
use super::{Viewer, WebResult};
use crate::domain::model::{Kind, MediaKind};
use crate::service::quizzes::{self, QuizView, SlotView};
use crate::service::{App, AppError};
use axum::Router;
use axum::extract::{Path, Query, State};
use axum::http::header;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use maud::{DOCTYPE, Markup, html};
use serde::Deserialize;

pub fn routes() -> Router<App> {
    Router::new()
        .route("/quizzes/{id}/print/host", get(host_sheet))
        .route("/quizzes/{id}/print/sheets", get(answer_sheets))
        .route("/quizzes/{id}/print/pictures", get(pictures))
        .route("/quizzes/{id}/present", get(present))
        .route("/quizzes/{id}/answers.csv", get(answers_csv))
}

async fn frozen(app: &App, id: i64) -> Result<QuizView, AppError> {
    let v = quizzes::view(app, id).await?;
    if v.quiz.frozen_at.is_none() {
        return Err(AppError::Conflict(
            "Freeze the quiz first, so the printout matches what you'll read.".into(),
        ));
    }
    Ok(v)
}

fn sheet(title: &str, class: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) }
                link rel="stylesheet" href="/static/app.css";
            }
            body class=(class) { main { (body) } }
        }
    }
}

fn number(v: &QuizView, s: &SlotView) -> String {
    let _ = v;
    format!("{}.{}", s.pos.0 + 1, s.pos.1 + 1)
}

async fn host_sheet(State(app): State<App>, _v: Viewer, Path(id): Path<i64>) -> WebResult<Markup> {
    let v = frozen(&app, id).await?;
    Ok(sheet(
        &format!("{} · host sheet", v.quiz.name),
        "print",
        html! {
            p.noprint { a href={"/quizzes/" (id)} { "← Back" } " · Print with your browser (Ctrl/Cmd+P)." }
            @for round in &v.rounds {
                section.round {
                    h1 { (v.quiz.name) " — " (round.title) }
                    @for s in &round.slots {
                        @if let Some(c) = &s.content {
                            div.q {
                                div { b { (number(&v, s)) " " } (c.prompt) " " span.muted.small { "(" (c.theme) ", " (stars(i64::from(c.difficulty))) ", " (c.points) " p)" } }
                                @if c.kind == Kind::MultipleChoice {
                                    div { @for (i, ch) in c.choices.iter().enumerate() { (char::from(b'A' + i as u8)) ") " (ch) "   " } }
                                }
                                @for m in &c.media {
                                    @if m.kind == MediaKind::Audio {
                                        div.small { "🎵 audio #" (m.media_id) @if let Some(st) = m.start_s { " from " (st) " s" } }
                                    } @else {
                                        img src={"/media/" (m.media_id)} alt="" style="max-height:30mm";
                                    }
                                }
                                @if c.kind == Kind::MultiPart {
                                    div.answer { @for p in &c.parts { (p.answer) " (" (p.points) ") · " } }
                                } @else {
                                    div.answer { "→ " (c.answer) }
                                }
                                @if !c.accepted_answers.is_empty() { div.small { "Also accept: " (c.accepted_answers.join(", ")) } }
                                @if !c.host_notes.is_empty() { div.small { "Note: " (c.host_notes) } }
                                @if !c.fun_fact.is_empty() { div.small { "💡 " (c.fun_fact) } }
                            }
                        }
                    }
                }
            }
        },
    ))
}

async fn answer_sheets(
    State(app): State<App>,
    _v: Viewer,
    Path(id): Path<i64>,
) -> WebResult<Markup> {
    let v = frozen(&app, id).await?;
    Ok(sheet(
        &format!("{} · answer sheets", v.quiz.name),
        "print",
        html! {
            p.noprint { a href={"/quizzes/" (id)} { "← Back" } " · One page per round. Print as many as there are teams." }
            @for round in &v.rounds {
                section.round {
                    div style="display:flex;justify-content:space-between;align-items:flex-end" {
                        h1 { (round.title) }
                        div { "Team: ______________________   Points: ____ / " (round.slots.iter().filter_map(|s| s.content.as_ref()).map(|c| c.points).sum::<u32>()) }
                    }
                    @for s in &round.slots {
                        @let lines = s.content.as_ref().map(|c| if c.kind == Kind::MultiPart { c.parts.len().max(1) } else { 1 }).unwrap_or(1);
                        @for i in 0..lines {
                            div.line { b { @if i == 0 { (number(&v, s)) } } }
                        }
                    }
                }
            }
        },
    ))
}

async fn pictures(State(app): State<App>, _v: Viewer, Path(id): Path<i64>) -> WebResult<Markup> {
    let v = frozen(&app, id).await?;
    let pics: Vec<(&SlotView, i64)> = v
        .rounds
        .iter()
        .flat_map(|r| r.slots.iter())
        .filter_map(|s| s.content.as_ref().map(|c| (s, c)))
        .flat_map(|(s, c)| {
            c.media
                .iter()
                .filter(|m| m.kind == MediaKind::Image)
                .map(move |m| (s, m.media_id))
        })
        .collect();
    Ok(sheet(
        &format!("{} · pictures", v.quiz.name),
        "print",
        html! {
            p.noprint { a href={"/quizzes/" (id)} { "← Back" } " · No answers on this page: hand one to each team." }
            @if pics.is_empty() { p { "This quiz has no picture questions." } }
            h1 { (v.quiz.name) }
            @for (s, media_id) in &pics {
                div.pic { b { (number(&v, s)) } img src={"/media/" (media_id)} alt=""; }
            }
        },
    ))
}

#[derive(Deserialize)]
struct PresentQ {
    i: Option<usize>,
}

async fn present(
    State(app): State<App>,
    _v: Viewer,
    Path(id): Path<i64>,
    Query(q): Query<PresentQ>,
) -> WebResult<Markup> {
    let v = frozen(&app, id).await?;
    let slots: Vec<(&str, &SlotView)> = v
        .rounds
        .iter()
        .flat_map(|r| r.slots.iter().map(move |s| (r.title.as_str(), s)))
        .collect();
    let i = q.i.unwrap_or(0).min(slots.len().saturating_sub(1));
    let Some((round, s)) = slots.get(i) else {
        return Err(AppError::NotFound.into());
    };
    let c = s.content.as_ref().ok_or(AppError::NotFound)?;
    Ok(sheet(
        &format!("{} · present", v.quiz.name),
        "present",
        html! {
            p.muted { (round) " · question " (number(&v, s)) " of " (slots.len()) }
            div.big { (c.prompt) }
            @if c.kind == Kind::MultipleChoice {
                ol type="A" style="font-size:30px" { @for ch in &c.choices { li { (ch) } } }
            }
            @for m in &c.media {
                @if m.kind == MediaKind::Image { img src={"/media/" (m.media_id)} alt="" style="max-height:55vh;max-width:100%;margin-top:16px"; }
                @else { audio controls src={"/media/" (m.media_id)} style="margin-top:16px" {} }
            }
            details {
                summary { "Show answer" }
                div.ans { (c.answer) }
                @if !c.fun_fact.is_empty() { p style="font-size:22px" { "💡 " (c.fun_fact) } }
            }
            div.nav {
                @if i > 0 { a href={"?i=" (i - 1)} { "← Previous" } }
                @if i + 1 < slots.len() { a href={"?i=" (i + 1)} { "Next →" } }
                a href={"/quizzes/" (id)} { "Exit" }
            }
        },
    ))
}

fn csv_field(s: &str) -> String {
    if s.contains([',', '"', '\n', '\r']) {
        format!("\"{}\"", s.replace('"', "\"\""))
    } else {
        s.to_string()
    }
}

pub fn csv(v: &QuizView) -> String {
    let mut out = String::from("number,round,question,answer,also_accepted,points\n");
    for round in &v.rounds {
        for s in &round.slots {
            if let Some(c) = &s.content {
                let answer = if c.kind == Kind::MultiPart {
                    c.parts
                        .iter()
                        .map(|p| p.answer.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                } else {
                    c.answer.clone()
                };
                let row = [
                    number(v, s),
                    round.title.clone(),
                    c.prompt.clone(),
                    answer,
                    c.accepted_answers.join("; "),
                    c.points.to_string(),
                ];
                out.push_str(
                    &row.iter()
                        .map(|f| csv_field(f))
                        .collect::<Vec<_>>()
                        .join(","),
                );
                out.push('\n');
            }
        }
    }
    out
}

async fn answers_csv(
    State(app): State<App>,
    _v: Viewer,
    Path(id): Path<i64>,
) -> WebResult<Response> {
    let v = frozen(&app, id).await?;
    let name = format!("attachment; filename=\"quiz-{id}-answers.csv\"");
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv; charset=utf-8".to_string()),
            (header::CONTENT_DISPOSITION, name),
        ],
        csv(&v),
    )
        .into_response())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quoting() {
        assert_eq!(csv_field("plain"), "plain");
        assert_eq!(csv_field("a,b"), "\"a,b\"");
        assert_eq!(csv_field("say \"hi\""), "\"say \"\"hi\"\"\"");
    }
}
