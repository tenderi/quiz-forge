//! Page chrome and shared components.

use crate::domain::model::{Content, Field, Kind};
use crate::service::members::Member;
use maud::{DOCTYPE, Markup, html};

/// htmx: swap error responses too (they're full pages), no eval.
const HTMX_CONFIG: &str = r#"{"allowEval":false,"selfRequestsOnly":true,"responseHandling":[{"code":"204","swap":false},{"code":"[2345]..","swap":true}]}"#;

fn head(title: &str) -> Markup {
    html! {
        head {
            meta charset="utf-8";
            meta name="viewport" content="width=device-width, initial-scale=1";
            meta name="robots" content="noindex, nofollow";
            meta name="htmx-config" content=(HTMX_CONFIG);
            title { (title) " · quiz-forge" }
            link rel="stylesheet" href="/static/app.css";
            script src="/static/htmx.min.js" {}
        }
    }
}

pub struct Nav<'a> {
    pub me: &'a Member,
    pub csrf: &'a str,
    pub active: &'a str,
    pub notice: Option<&'a str>,
}

pub fn page(title: &str, nav: Nav<'_>, body: Markup) -> Markup {
    let link = |href: &str, label: &str, key: &str| {
        html! { a href=(href) class=[(nav.active == key).then_some("on")] { (label) } }
    };
    let hx_headers = format!(r#"{{"X-CSRF-Token":"{}"}}"#, nav.csrf);
    html! {
        (DOCTYPE)
        html lang="en" {
            (head(title))
            body hx-boost="true" hx-headers=(hx_headers) {
                header.top {
                    a.brand href="/" { "quiz-forge" }
                    nav {
                        (link("/questions", "Bank", "bank"))
                        (link("/questions?needs_vote=1", "Vote", "vote"))
                        (link("/questions/new", "Submit", "new"))
                        (link("/quizzes", "Quizzes", "quizzes"))
                        (link("/templates", "Templates", "templates"))
                        (link("/events", "Events", "events"))
                        (link("/players", "Players", "players"))
                        (link("/settings", "Settings", "settings"))
                    }
                    div.me {
                        span { (nav.me.display_name) }
                        form.inline method="post" action="/logout" { button.small type="submit" { "Sign out" } }
                    }
                }
                main {
                    @if let Some(n) = nav.notice { div.notice { (n) } }
                    (body)
                }
            }
        }
    }
}

pub fn bare(title: &str, body: Markup) -> Markup {
    html! {
        (DOCTYPE)
        html lang="en" { (head(title)) body { (body) } }
    }
}

pub fn status_badge(status: &str) -> Markup {
    html! { span class={"badge " (status)} { (status.replace('_', " ")) } }
}

pub fn stars(d: i64) -> String {
    let d = d.clamp(0, 5) as usize;
    "★".repeat(d) + &"☆".repeat(5 - d)
}

pub fn meta_chips(c: &Content) -> Markup {
    html! {
        span.chip { (c.kind.label()) }
        span.chip { (c.theme) }
        span.chip title={"difficulty " (c.difficulty)} { (stars(i64::from(c.difficulty))) }
        span.chip { (c.language) }
        @for t in &c.tags { span.chip { "#" (t) } }
    }
}

pub fn media_view(c: &Content) -> Markup {
    html! {
        @if !c.media.is_empty() {
            div.media {
                @for m in &c.media {
                    @match m.kind {
                        crate::domain::model::MediaKind::Image => img src={"/media/" (m.media_id)} alt="question image" loading="lazy";
                        crate::domain::model::MediaKind::Audio => audio controls preload="none" src={"/media/" (m.media_id)} {}
                    }
                }
            }
        }
    }
}

/// The answer and everything around it, collapsed so browsing doesn't spoil.
pub fn answer_block(c: &Content, open: bool) -> Markup {
    html! {
        details.answer open[open] {
            summary { "Answer" }
            @if c.kind == Kind::MultiPart {
                ol { @for p in &c.parts { li { @if let Some(pp) = &p.prompt { (pp) ": " } b { (p.answer) } " (" (p.points) " p)" } } }
            } @else {
                p { b { (c.answer) } @if c.points > 1 { " (" (c.points) " p)" } }
            }
            @if !c.accepted_answers.is_empty() { p.small { "Also accepted: " (c.accepted_answers.join(", ")) } }
            @if !c.fun_fact.is_empty() { p { "💡 " (c.fun_fact) } }
            @if !c.host_notes.is_empty() { p.small.muted { "Host notes: " (c.host_notes) } }
        }
    }
}

pub fn content_view(c: &Content, open_answer: bool) -> Markup {
    html! {
        div.row { (meta_chips(c)) }
        p.prompt { (c.prompt) }
        @if c.kind == Kind::MultipleChoice {
            ol type="A" { @for ch in &c.choices { li { (ch) } } }
        }
        (media_view(c))
        (answer_block(c, open_answer))
        @if !c.background.is_empty() {
            details { summary { "Background" } p { (c.background) } }
        }
        @if !c.sources.is_empty() {
            p.small { "Sources: "
                @for (i, s) in c.sources.iter().enumerate() {
                    @if i > 0 { ", " }
                    @if s.starts_with("http://") || s.starts_with("https://") { a href=(s) rel="noopener noreferrer" target="_blank" { (s) } } @else { (s) }
                }
            }
        }
    }
}

fn value_text(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        serde_json::Value::Array(a) => a
            .iter()
            .map(|x| match x {
                serde_json::Value::String(s) => s.clone(),
                serde_json::Value::Object(o) => o
                    .get("answer")
                    .and_then(|a| a.as_str())
                    .map(String::from)
                    .or_else(|| o.get("media_id").map(|m| format!("media #{m}")))
                    .unwrap_or_else(|| x.to_string()),
                other => other.to_string(),
            })
            .collect::<Vec<_>>()
            .join(" · "),
        other => other.to_string(),
    }
}

/// Word-level diff of two strings as <del>/<ins>.
pub fn word_diff(old: &str, new: &str) -> Markup {
    let diff = similar::TextDiff::from_words(old, new);
    html! {
        @for change in diff.iter_all_changes() {
            @match change.tag() {
                similar::ChangeTag::Equal => (change.value()),
                similar::ChangeTag::Delete => del { (change.value()) },
                similar::ChangeTag::Insert => ins { (change.value()) },
            }
        }
    }
}

pub fn changes_table(changes: &[(Field, serde_json::Value, serde_json::Value)]) -> Markup {
    html! {
        table {
            tr { th { "Field" } th { "Change" } }
            @for (f, old, new) in changes {
                tr {
                    td { (f.label()) }
                    td { (word_diff(&value_text(old), &value_text(new))) }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn diff_and_values() {
        let m = word_diff("the long river", "the longest river").into_string();
        assert!(m.contains("<del>long</del>") && m.contains("<ins>longest</ins>"));
        assert_eq!(value_text(&serde_json::json!(["a", "b"])), "a · b");
        assert_eq!(
            value_text(&serde_json::json!([{"answer": "x", "points": 1}])),
            "x"
        );
        assert_eq!(
            value_text(&serde_json::json!([{"media_id": 3, "kind": "image"}])),
            "media #3"
        );
        assert_eq!(value_text(&serde_json::json!(3)), "3");
        assert_eq!(stars(9), "★★★★★");
        assert!(
            status_badge("time_sensitive")
                .into_string()
                .contains("time sensitive")
        );
    }
}
