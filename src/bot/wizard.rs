//! The `/add` conversation in a private chat (DESIGN.md §5.2), as a pure state
//! machine: (state, input) → (state, reply, action). The Telegram glue stores
//! the state in `wizard_state` and does whatever the action says.

use crate::domain::model::{Content, Kind, MediaKind, MediaRef, format_parts, parse_parts};
use crate::service::{Button, esc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Kind,
    Prompt,
    Answer,
    Choices,
    Parts,
    Media,
    Difficulty,
    Theme,
    Language,
    Extras,
    Accepted,
    FunFact,
    Background,
    HostNotes,
    Sources,
    Tags,
    Review,
    ConfirmDuplicates,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Wizard {
    pub step: Step,
    pub draft: Content,
    /// Editing one field from the review screen: go back there afterwards.
    #[serde(default)]
    pub back_to_review: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Input {
    Text(String),
    /// Callback data without the `w:` prefix.
    Button(String),
    Media {
        media_id: i64,
        kind: MediaKind,
    },
}

#[derive(Debug, Clone, PartialEq, Default)]
pub struct Reply {
    pub html: String,
    pub buttons: Vec<Vec<Button>>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    None,
    Submit { confirm_duplicates: bool },
    Cancel,
}

fn b(text: &str, data: &str) -> Button {
    Button::Callback {
        text: text.into(),
        data: format!("w:{data}"),
    }
}

fn reply(html: impl Into<String>, buttons: Vec<Vec<Button>>) -> Reply {
    Reply {
        html: html.into(),
        buttons,
    }
}

fn cancel_row() -> Vec<Button> {
    vec![b("✖ Cancel", "cancel")]
}

pub fn start(default_language: &str, default_theme: &str) -> (Wizard, Reply) {
    let w = Wizard {
        step: Step::Kind,
        draft: Content::text("", "", default_theme, default_language),
        back_to_review: false,
    };
    let r = prompt_for(&w, &[]);
    (w, r)
}

/// What to say when arriving at the wizard's current step.
pub fn prompt_for(w: &Wizard, themes: &[String]) -> Reply {
    match w.step {
        Step::Kind => {
            let rows: Vec<Vec<Button>> = Kind::ALL
                .chunks(2)
                .map(|ks| {
                    ks.iter()
                        .map(|k| b(k.label(), &format!("kind:{}", k.as_str())))
                        .collect()
                })
                .collect();
            let mut rows = rows;
            rows.push(cancel_row());
            reply(
                "New question. What kind is it?\n<i>Remember: no yes/no questions, and numbers only in tiebreakers (years are fine).</i>",
                rows,
            )
        }
        Step::Prompt => reply(
            "Send the question, as you'd read it out.",
            vec![cancel_row()],
        ),
        Step::Answer => reply("Send the answer.", vec![cancel_row()]),
        Step::Choices => reply(
            format!(
                "Send the choices, one per line (3–6). One of them must be the answer: <b>{}</b>",
                esc(&w.draft.answer)
            ),
            vec![cancel_row()],
        ),
        Step::Parts => reply(
            "Send the parts, one per line: <code>answer</code>, or <code>prompt :: answer ; points</code>.",
            vec![cancel_row()],
        ),
        Step::Media => {
            let what = if w.draft.kind == Kind::Audio {
                "an audio file"
            } else {
                "the picture(s)"
            };
            let got = w.draft.media.len();
            reply(
                format!(
                    "Send {what}. {}",
                    if got > 0 {
                        format!("Got {got} so far.")
                    } else {
                        String::new()
                    }
                ),
                vec![vec![b("Done", "media_done")], cancel_row()],
            )
        }
        Step::Difficulty => reply(
            "How hard is it? 1 = everyone knows, 5 = specialists only.",
            vec![
                (1..=5)
                    .map(|d| b(&d.to_string(), &format!("diff:{d}")))
                    .collect(),
                cancel_row(),
            ],
        ),
        Step::Theme => {
            let mut rows: Vec<Vec<Button>> = themes
                .chunks(3)
                .enumerate()
                .map(|(ci, ts)| {
                    ts.iter()
                        .enumerate()
                        .map(|(i, t)| b(t, &format!("theme:{}", ci * 3 + i)))
                        .collect()
                })
                .collect();
            rows.push(cancel_row());
            reply(
                "Which theme? (Or type a theme that exists in the web app.)",
                rows,
            )
        }
        Step::Language => reply(
            format!(
                "Language of this question? Type a code like en or sv, or keep <b>{}</b>.",
                esc(&w.draft.language)
            ),
            vec![
                vec![b(&format!("Keep {}", w.draft.language), "lang_keep")],
                cancel_row(),
            ],
        ),
        Step::Extras => reply(
            "Anything else? All optional.",
            vec![
                vec![
                    b("Also accepted", "x:accepted"),
                    b("Fun fact", "x:fun_fact"),
                ],
                vec![
                    b("Background", "x:background"),
                    b("Host notes", "x:host_notes"),
                ],
                vec![b("Sources", "x:sources"), b("Tags", "x:tags")],
                vec![b("✅ Review", "review")],
            ],
        ),
        Step::Accepted => reply("Other answers to accept, one per line.", vec![]),
        Step::FunFact => reply("A fun fact to read after the answer.", vec![]),
        Step::Background => reply("The longer background story, for the host.", vec![]),
        Step::HostNotes => reply(
            "Notes for the host (pronunciation, what to accept).",
            vec![],
        ),
        Step::Sources => reply("Source URLs, one per line.", vec![]),
        Step::Tags => reply("Tags, separated by commas.", vec![]),
        Step::Review => review(w),
        Step::ConfirmDuplicates => reply(
            "Submit anyway?",
            vec![vec![
                b("Yes, it's different", "dup_yes"),
                b("✖ Cancel", "cancel"),
            ]],
        ),
    }
}

fn review(w: &Wizard) -> Reply {
    let c = &w.draft;
    let mut h = format!(
        "<b>Review</b>\n{} · {} · difficulty {} · {}\n\n<b>{}</b>\n",
        c.kind.label(),
        esc(&c.theme),
        c.difficulty,
        esc(&c.language),
        esc(&c.prompt)
    );
    if c.kind == Kind::MultipleChoice {
        for (i, ch) in c.choices.iter().enumerate() {
            h.push_str(&format!("{}) {}\n", (b'A' + i as u8) as char, esc(ch)));
        }
    }
    if c.kind == Kind::MultiPart {
        h.push_str(&format!("Parts:\n{}\n", esc(&format_parts(&c.parts))));
    } else {
        h.push_str(&format!("Answer: {}\n", esc(&c.answer)));
    }
    if !c.media.is_empty() {
        h.push_str(&format!("Media: {}\n", c.media.len()));
    }
    for (label, v) in [
        ("Also accepted", c.accepted_answers.join(", ")),
        ("Fun fact", c.fun_fact.clone()),
        ("Background", c.background.clone()),
        ("Host notes", c.host_notes.clone()),
        ("Sources", c.sources.join(" ")),
        ("Tags", c.tags.join(", ")),
    ] {
        if !v.is_empty() {
            h.push_str(&format!("{label}: {}\n", esc(&v)));
        }
    }
    let mut fix = vec![b("Question", "edit:prompt")];
    match c.kind {
        Kind::MultiPart => fix.push(b("Parts", "edit:parts")),
        Kind::MultipleChoice => {
            fix.push(b("Answer", "edit:answer"));
            fix.push(b("Choices", "edit:choices"));
        }
        _ => fix.push(b("Answer", "edit:answer")),
    }
    reply(
        h,
        vec![
            vec![b("🚀 Submit", "submit")],
            fix,
            vec![
                b("Difficulty", "edit:difficulty"),
                b("Theme", "edit:theme"),
                b(&format!("Language: {}", c.language), "edit:language"),
            ],
            vec![b("Extras", "edit:extras")],
            cancel_row(),
        ],
    )
}

fn lines(s: &str) -> Vec<String> {
    s.lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

/// The step after `cur` in the normal flow.
fn next(w: &Wizard, cur: Step) -> Step {
    if w.back_to_review && cur != Step::Kind {
        return Step::Review;
    }
    match cur {
        Step::Kind => Step::Prompt,
        Step::Prompt => {
            if w.draft.kind == Kind::MultiPart {
                Step::Parts
            } else {
                Step::Answer
            }
        }
        Step::Answer => match w.draft.kind {
            Kind::MultipleChoice => Step::Choices,
            Kind::Image | Kind::Audio => Step::Media,
            _ => Step::Difficulty,
        },
        Step::Choices | Step::Parts | Step::Media => Step::Difficulty,
        Step::Difficulty => Step::Theme,
        // The language is the group's default; it's changed from the review screen.
        Step::Theme => Step::Extras,
        Step::Language => Step::Extras,
        Step::Accepted
        | Step::FunFact
        | Step::Background
        | Step::HostNotes
        | Step::Sources
        | Step::Tags => Step::Extras,
        Step::Extras | Step::Review | Step::ConfirmDuplicates => Step::Review,
    }
}

fn go_to(mut w: Wizard, step: Step, themes: &[String]) -> (Wizard, Reply, Action) {
    if step == Step::Review || step == Step::Extras {
        w.back_to_review = false;
    }
    w.step = step;
    let r = prompt_for(&w, themes);
    (w, r, Action::None)
}

fn again(w: Wizard, note: &str, themes: &[String]) -> (Wizard, Reply, Action) {
    let mut r = prompt_for(&w, themes);
    r.html = format!("{note}\n\n{}", r.html);
    (w, r, Action::None)
}

pub fn step(mut w: Wizard, input: Input, themes: &[String]) -> (Wizard, Reply, Action) {
    if input == Input::Button("cancel".into()) {
        return (
            w,
            reply("Cancelled. Nothing was submitted.", vec![]),
            Action::Cancel,
        );
    }
    let cur = w.step;
    match (cur, input) {
        (Step::Kind, Input::Button(d)) if d.starts_with("kind:") => match d[5..].parse::<Kind>() {
            Ok(k) => {
                w.draft.kind = k;
                let n = next(&w, cur);
                go_to(w, n, themes)
            }
            Err(_) => again(w, "Pick one of the buttons.", themes),
        },
        (Step::Prompt, Input::Text(t)) if !t.trim().is_empty() => {
            w.draft.prompt = t.trim().to_string();
            let n = next(&w, cur);
            go_to(w, n, themes)
        }
        (Step::Answer, Input::Text(t)) if !t.trim().is_empty() => {
            w.draft.answer = t.trim().to_string();
            let n = next(&w, cur);
            go_to(w, n, themes)
        }
        (Step::Choices, Input::Text(t)) => {
            let ch = lines(&t);
            if !(3..=6).contains(&ch.len()) {
                return again(w, "That needs 3–6 lines.", themes);
            }
            w.draft.choices = ch;
            let n = next(&w, cur);
            go_to(w, n, themes)
        }
        (Step::Parts, Input::Text(t)) => {
            let parts = parse_parts(&t);
            if parts.len() < 2 {
                return again(w, "A multi-part question needs at least two parts.", themes);
            }
            w.draft.parts = parts;
            w.draft.answer.clear();
            w.draft = w.draft.clone().normalized();
            let n = next(&w, cur);
            go_to(w, n, themes)
        }
        (Step::Media, Input::Media { media_id, kind }) => {
            let want = if w.draft.kind == Kind::Audio {
                MediaKind::Audio
            } else {
                MediaKind::Image
            };
            if kind != want {
                return again(
                    w,
                    "That's not the right kind of file for this question.",
                    themes,
                );
            }
            if want == MediaKind::Audio {
                w.draft.media.clear();
            }
            if !w.draft.media.iter().any(|m| m.media_id == media_id) {
                w.draft.media.push(MediaRef {
                    media_id,
                    kind,
                    start_s: None,
                    end_s: None,
                });
            }
            go_to(w, Step::Media, themes)
        }
        (Step::Media, Input::Button(d)) if d == "media_done" => {
            if w.draft.media.is_empty() {
                return again(w, "Send at least one file first.", themes);
            }
            let n = next(&w, cur);
            go_to(w, n, themes)
        }
        (Step::Difficulty, Input::Button(d)) if d.starts_with("diff:") => {
            match d[5..].parse::<u8>() {
                Ok(x) if (1..=5).contains(&x) => {
                    w.draft.difficulty = x;
                    let n = next(&w, cur);
                    go_to(w, n, themes)
                }
                _ => again(w, "Pick 1–5.", themes),
            }
        }
        (Step::Theme, Input::Button(d)) if d.starts_with("theme:") => {
            match d[6..].parse::<usize>().ok().and_then(|i| themes.get(i)) {
                Some(t) => {
                    w.draft.theme = t.clone();
                    let n = next(&w, cur);
                    go_to(w, n, themes)
                }
                None => again(w, "That theme is gone; pick another.", themes),
            }
        }
        (Step::Theme, Input::Text(t)) => {
            let t = crate::service::questions::normalize_theme(&t);
            if themes.contains(&t) {
                w.draft.theme = t;
                let n = next(&w, cur);
                go_to(w, n, themes)
            } else {
                again(
                    w,
                    "No such theme. New themes can be added in the web app.",
                    themes,
                )
            }
        }
        (Step::Language, Input::Button(d)) if d == "lang_keep" => {
            let n = next(&w, cur);
            go_to(w, n, themes)
        }
        (Step::Language, Input::Text(t)) => {
            let l = t.trim().to_lowercase();
            if !crate::domain::rules::language_ok(&l) {
                return again(w, "Use a short code like fi, en or sv.", themes);
            }
            w.draft.language = l;
            let n = next(&w, cur);
            go_to(w, n, themes)
        }
        (Step::Extras, Input::Button(d)) if d.starts_with("x:") => {
            let s = match &d[2..] {
                "accepted" => Step::Accepted,
                "fun_fact" => Step::FunFact,
                "background" => Step::Background,
                "host_notes" => Step::HostNotes,
                "sources" => Step::Sources,
                "tags" => Step::Tags,
                _ => return again(w, "Pick one of the buttons.", themes),
            };
            go_to(w, s, themes)
        }
        (Step::Extras, Input::Button(d)) if d == "review" => go_to(w, Step::Review, themes),
        (Step::Accepted, Input::Text(t)) => {
            w.draft.accepted_answers = lines(&t);
            go_to(w, Step::Extras, themes)
        }
        (Step::FunFact, Input::Text(t)) => {
            w.draft.fun_fact = t.trim().to_string();
            go_to(w, Step::Extras, themes)
        }
        (Step::Background, Input::Text(t)) => {
            w.draft.background = t.trim().to_string();
            go_to(w, Step::Extras, themes)
        }
        (Step::HostNotes, Input::Text(t)) => {
            w.draft.host_notes = t.trim().to_string();
            go_to(w, Step::Extras, themes)
        }
        (Step::Sources, Input::Text(t)) => {
            w.draft.sources = lines(&t);
            go_to(w, Step::Extras, themes)
        }
        (Step::Tags, Input::Text(t)) => {
            w.draft.tags = t.split(',').map(String::from).collect();
            w.draft = w.draft.clone().normalized();
            go_to(w, Step::Extras, themes)
        }
        (Step::Review, Input::Button(d)) if d == "submit" => {
            let r = reply("Submitting…", vec![]);
            (
                w,
                r,
                Action::Submit {
                    confirm_duplicates: false,
                },
            )
        }
        (Step::Review, Input::Button(d)) if d.starts_with("edit:") => {
            let s = match &d[5..] {
                "prompt" => Step::Prompt,
                "answer" => Step::Answer,
                "choices" => Step::Choices,
                "parts" => Step::Parts,
                "difficulty" => Step::Difficulty,
                "theme" => Step::Theme,
                "language" => Step::Language,
                "extras" => return go_to(w, Step::Extras, themes),
                _ => return again(w, "Pick one of the buttons.", themes),
            };
            w.back_to_review = true;
            w.step = s;
            let r = prompt_for(&w, themes);
            (w, r, Action::None)
        }
        (Step::ConfirmDuplicates, Input::Button(d)) if d == "dup_yes" => {
            let r = reply("Submitting…", vec![]);
            (
                w,
                r,
                Action::Submit {
                    confirm_duplicates: true,
                },
            )
        }
        _ => {
            let hint = match cur {
                Step::Kind
                | Step::Difficulty
                | Step::Extras
                | Step::Review
                | Step::ConfirmDuplicates => "Use the buttons, please.",
                Step::Media => "Send a file, or press Done.",
                _ => "Send that as a text message.",
            };
            again(w, hint, themes)
        }
    }
}

/// After the service refused a submission or found duplicates.
pub fn after_refusal(
    mut w: Wizard,
    problems: &str,
    duplicates: bool,
    themes: &[String],
) -> (Wizard, Reply) {
    w.step = if duplicates {
        Step::ConfirmDuplicates
    } else {
        Step::Review
    };
    let mut r = prompt_for(&w, themes);
    r.html = format!("{problems}\n\n{}", r.html);
    (w, r)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn themes() -> Vec<String> {
        vec!["geography".into(), "music".into(), "history".into()]
    }

    fn run(w: Wizard, inputs: Vec<Input>) -> (Wizard, Reply, Action) {
        let mut state = (w, Reply::default(), Action::None);
        for i in inputs {
            state = step(state.0, i, &themes());
        }
        state
    }

    fn t(s: &str) -> Input {
        Input::Text(s.into())
    }

    fn btn(s: &str) -> Input {
        Input::Button(s.into())
    }

    #[test]
    fn text_question_happy_path() {
        let (w, r) = start("fi", "geography");
        assert!(r.html.contains("What kind"));
        let (w, r, a) = run(
            w,
            vec![
                btn("kind:text"),
                t("Mikä on Suomen pääkaupunki?"),
                t("Helsinki"),
                btn("diff:2"),
                btn("theme:0"),
            ],
        );
        assert_eq!(w.step, Step::Extras);
        assert_eq!(a, Action::None);
        assert!(
            r.buttons
                .iter()
                .flatten()
                .any(|b| matches!(b, Button::Callback { data, .. } if data == "w:review"))
        );
        let (w, _, _) = run(
            w,
            vec![
                btn("x:fun_fact"),
                t("Perustettu 1550"),
                btn("x:tags"),
                t("capitals, Nordic"),
                btn("x:accepted"),
                t("Helsingfors"),
                btn("review"),
            ],
        );
        assert_eq!(w.step, Step::Review);
        assert_eq!(w.draft.fun_fact, "Perustettu 1550");
        assert_eq!(w.draft.tags, vec!["capitals", "nordic"]);
        let r = prompt_for(&w, &themes());
        assert!(r.html.contains("Answer: Helsinki") && r.html.contains("Fun fact"));
        let (_, _, a) = step(w, btn("submit"), &themes());
        assert_eq!(
            a,
            Action::Submit {
                confirm_duplicates: false
            }
        );
    }

    #[test]
    fn multiple_choice_and_multi_part_and_media() {
        let (w, _) = start("fi", "geography");
        let (w, _, _) = run(
            w,
            vec![
                btn("kind:multiple_choice"),
                t("Largest lake?"),
                t("Saimaa"),
                t("Saimaa\nInari"),
            ],
        );
        assert_eq!(w.step, Step::Choices, "too few choices");
        let (w, _, _) = run(w, vec![t("Saimaa\nInari\nPäijänne"), btn("diff:9")]);
        assert_eq!(w.step, Step::Difficulty);
        assert_eq!(w.draft.choices.len(), 3);

        let (w, _) = start("fi", "geography");
        let (w, _, _) = run(
            w,
            vec![
                btn("kind:multi_part"),
                t("Name the Nordic capitals"),
                t("Oslo"),
            ],
        );
        assert_eq!(w.step, Step::Parts);
        let (w, _, _) = run(w, vec![t("Oslo\nHelsinki ; 2")]);
        assert_eq!(w.step, Step::Difficulty);
        assert_eq!(w.draft.points, 3);
        assert_eq!(w.draft.answer, "Oslo, Helsinki");

        let (w, _) = start("fi", "geography");
        let (w, r, _) = run(
            w,
            vec![
                btn("kind:image"),
                t("Who?"),
                t("Sibelius"),
                btn("media_done"),
            ],
        );
        assert!(r.html.contains("at least one file"));
        let (w, r, _) = run(
            w,
            vec![Input::Media {
                media_id: 7,
                kind: MediaKind::Audio,
            }],
        );
        assert!(r.html.contains("not the right kind"));
        let (w, r, _) = run(
            w,
            vec![
                Input::Media {
                    media_id: 7,
                    kind: MediaKind::Image,
                },
                Input::Media {
                    media_id: 7,
                    kind: MediaKind::Image,
                },
                Input::Media {
                    media_id: 8,
                    kind: MediaKind::Image,
                },
            ],
        );
        assert!(r.html.contains("Got 2"));
        let (w, _, _) = run(w, vec![btn("media_done")]);
        assert_eq!(w.step, Step::Difficulty);

        let (w, _) = start("fi", "geography");
        let (w, _, _) = run(
            w,
            vec![
                btn("kind:audio"),
                t("What song?"),
                t("Finlandia"),
                Input::Media {
                    media_id: 1,
                    kind: MediaKind::Audio,
                },
                Input::Media {
                    media_id: 2,
                    kind: MediaKind::Audio,
                },
            ],
        );
        assert_eq!(w.draft.media.len(), 1, "one clip only; the newest wins");
        assert_eq!(w.draft.media[0].media_id, 2);
    }

    #[test]
    fn theme_language_and_bad_inputs() {
        let (w, _) = start("fi", "geography");
        let (w, r, _) = run(w, vec![t("hello")]);
        assert!(r.html.contains("Use the buttons"));
        let (w, r, _) = run(w, vec![btn("kind:nope")]);
        assert!(r.html.contains("Pick one"));
        let (w, _, _) = run(
            w,
            vec![btn("kind:text"), t("  "), t("Q?"), t("A"), btn("diff:3")],
        );
        assert_eq!(w.draft.prompt, "Q?");
        let (w, r, _) = run(w.clone(), vec![btn("theme:42")]);
        assert!(r.html.contains("gone"));
        let (w2, r, _) = run(w.clone(), vec![t("astrology")]);
        assert!(r.html.contains("No such theme"));
        assert_eq!(w2.step, Step::Theme);
        let (w, _, _) = run(w, vec![t("Music")]);
        assert_eq!(w.draft.theme, "music");
        // the group's language is taken as is: straight on to the extras
        assert_eq!(w.step, Step::Extras);
        assert_eq!(w.draft.language, "fi");
        // and changed from the review screen when one question differs
        let (w, r, _) = run(w, vec![btn("review")]);
        assert!(r.buttons.iter().flatten().any(
            |b| matches!(b, Button::Callback { text, data } if text == "Language: fi" && data == "w:edit:language")
        ));
        let (w, r, _) = run(w, vec![btn("edit:language"), t("not a language!")]);
        assert!(r.html.contains("short code"));
        assert_eq!(w.step, Step::Language);
        let (w, _, _) = run(w, vec![t("EN")]);
        assert_eq!((w.step, w.draft.language.as_str()), (Step::Review, "en"));
        let (w, _, _) = run(w, vec![btn("edit:language"), btn("lang_keep")]);
        assert_eq!((w.step, w.draft.language.as_str()), (Step::Review, "en"));
        let (w, r, _) = run(w, vec![btn("edit:extras"), btn("x:nope")]);
        assert!(r.html.contains("Pick one"));
        let (w, _, _) = run(
            w,
            vec![
                btn("x:background"),
                t("Long story"),
                btn("x:host_notes"),
                t("Say slowly"),
                btn("x:sources"),
                t("https://a\nhttps://b"),
            ],
        );
        assert_eq!(w.draft.sources.len(), 2);
        assert_eq!(w.draft.host_notes, "Say slowly");
        assert_eq!(w.draft.background, "Long story");
    }

    #[test]
    fn editing_from_review_returns_to_review() {
        let (w, _) = start("fi", "geography");
        let (w, _, _) = run(
            w,
            vec![
                btn("kind:text"),
                t("Q?"),
                t("A"),
                btn("diff:3"),
                btn("theme:1"),
                btn("review"),
            ],
        );
        assert_eq!(w.step, Step::Review);
        let (w, _, _) = run(w, vec![btn("edit:answer"), t("B")]);
        assert_eq!(w.step, Step::Review);
        assert_eq!(w.draft.answer, "B");
        let (w, _, _) = run(
            w,
            vec![
                btn("edit:difficulty"),
                btn("diff:5"),
                btn("edit:theme"),
                btn("theme:2"),
                btn("edit:prompt"),
                t("Q2?"),
            ],
        );
        assert_eq!(
            (
                w.step,
                w.draft.difficulty,
                w.draft.theme.as_str(),
                w.draft.prompt.as_str()
            ),
            (Step::Review, 5, "history", "Q2?")
        );
        let (w, _, _) = run(w, vec![btn("edit:extras")]);
        assert_eq!(w.step, Step::Extras);
        let (w, _, _) = run(w, vec![btn("review"), btn("edit:bogus")]);
        assert_eq!(w.step, Step::Review);
        let (w2, _, _) = run(w.clone(), vec![btn("edit:choices"), t("a\nb\nB")]);
        assert_eq!(w2.step, Step::Review);
        let (w3, _, _) = run(w, vec![btn("edit:parts"), t("x\ny")]);
        assert_eq!(w3.step, Step::Review);
    }

    #[test]
    fn refusal_duplicates_and_cancel() {
        let (w, _) = start("fi", "geography");
        let (w, _, _) = run(
            w,
            vec![
                btn("kind:text"),
                t("Q?"),
                t("A"),
                btn("diff:3"),
                btn("theme:1"),
                btn("review"),
            ],
        );
        let (w2, r) = after_refusal(
            w.clone(),
            "Yes/no questions aren't allowed.",
            false,
            &themes(),
        );
        assert_eq!(w2.step, Step::Review);
        assert!(r.html.starts_with("Yes/no"));
        let (w3, r) = after_refusal(w.clone(), "Similar: #3", true, &themes());
        assert_eq!(w3.step, Step::ConfirmDuplicates);
        assert!(r.html.contains("Submit anyway"));
        let (_, _, a) = step(w3.clone(), btn("dup_yes"), &themes());
        assert_eq!(
            a,
            Action::Submit {
                confirm_duplicates: true
            }
        );
        let (_, r, a) = step(w3, btn("cancel"), &themes());
        assert_eq!(a, Action::Cancel);
        assert!(r.html.contains("Cancelled"));
        // every step has a prompt
        for s in [
            Step::Kind,
            Step::Prompt,
            Step::Answer,
            Step::Choices,
            Step::Parts,
            Step::Media,
            Step::Difficulty,
            Step::Theme,
            Step::Language,
            Step::Extras,
            Step::Accepted,
            Step::FunFact,
            Step::Background,
            Step::HostNotes,
            Step::Sources,
            Step::Tags,
            Step::Review,
            Step::ConfirmDuplicates,
        ] {
            let mut x = w.clone();
            x.step = s;
            assert!(!prompt_for(&x, &themes()).html.is_empty());
        }
        let (_, r, _) = run(w.clone(), vec![btn("edit:prompt"), btn("nope")]);
        assert!(r.html.contains("text message"));
        let mut m = w;
        m.step = Step::Media;
        let (_, r, _) = step(m, t("x"), &themes());
        assert!(r.html.contains("press Done"));
    }

    #[test]
    fn state_serializes() {
        let (w, _) = start("fi", "geography");
        let json = serde_json::to_string(&w).unwrap();
        let back: Wizard = serde_json::from_str(&json).unwrap();
        assert_eq!(back, w);
    }
}
