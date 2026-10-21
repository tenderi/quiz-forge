//! Telegram message rendering: vote cards and house-rule reactions.

use super::questions::{self, round_row, round_votes};
use super::{App, Button, OutMessage, Result, esc};
use crate::db::parse_ts;
use crate::domain::dedupe::{Band, Match};
use crate::domain::model::{Content, Kind};
use crate::domain::rules::Reaction;
use crate::domain::text::stable_pick;
use crate::domain::voting::Vote;
use serde::Deserialize;
use std::sync::LazyLock;

#[derive(Deserialize)]
struct Reactions {
    boo: String,
    shame: Vec<String>,
}

static REACTIONS: LazyLock<Reactions> = LazyLock::new(|| {
    toml::from_str(include_str!("../../data/reactions.toml")).expect("data/reactions.toml is valid")
});

/// The group message for a house-rule reaction. The shaming line is picked by
/// hash of the prompt, so the same attempt always gets the same one.
pub fn reaction(r: Reaction, name: &str, prompt: &str) -> String {
    let template = match r {
        Reaction::Boo => REACTIONS.boo.as_str(),
        Reaction::Shame => {
            REACTIONS.shame[stable_pick(&["shame", prompt], REACTIONS.shame.len())].as_str()
        }
    };
    template
        .replace("{name}", &esc(name))
        .replace("{prompt}", &esc(prompt))
}

/// "\n<a href=…>Open in the web app</a>", or nothing when the instance has no
/// public https address (Telegram rejects links to localhost).
pub fn web_link(app: &App, path: &str) -> String {
    if app.cfg.https() {
        format!(
            "\n<a href=\"{}\">Open in the web app</a>",
            esc(&app.cfg.url(path))
        )
    } else {
        String::new()
    }
}

#[derive(Debug, Clone)]
pub struct CardData {
    pub question_id: i64,
    pub round: i64,
    pub content: Content,
    pub author_name: String,
    pub votes: Vec<(String, Vote)>,
    pub closes_at: String,
    pub outcome: Option<String>,
    pub duplicates: Vec<Match>,
    pub web_url: Option<String>,
}

fn difficulty_stars(d: u8) -> String {
    "★".repeat(d as usize) + &"☆".repeat(5usize.saturating_sub(d as usize))
}

pub fn vote_card(d: &CardData) -> OutMessage {
    let c = &d.content;
    let mut h = String::new();
    let round = if d.round > 1 {
        format!(" (round {})", d.round)
    } else {
        String::new()
    };
    h.push_str(&format!(
        "🧠 <b>Question #{}</b>{round} · {} · {} · {} · {}\n\n",
        d.question_id,
        c.kind.label(),
        esc(&c.theme),
        difficulty_stars(c.difficulty),
        esc(&c.language)
    ));
    h.push_str(&format!("<b>{}</b>\n", esc(&c.prompt)));
    if c.kind == Kind::MultipleChoice {
        for (i, ch) in c.choices.iter().enumerate() {
            h.push_str(&format!("{}) {}\n", (b'A' + i as u8) as char, esc(ch)));
        }
    }
    if !c.media.is_empty() {
        h.push_str(&format!("📎 {} media file(s) above\n", c.media.len()));
    }
    if c.kind == Kind::MultiPart {
        let parts: Vec<String> = c
            .parts
            .iter()
            .map(|p| match &p.prompt {
                Some(pp) => format!("{}: {} ({}p)", esc(pp), esc(&p.answer), p.points),
                None => format!("{} ({}p)", esc(&p.answer), p.points),
            })
            .collect();
        h.push_str(&format!(
            "\nAnswer: <tg-spoiler>{}</tg-spoiler>\n",
            parts.join("; ")
        ));
    } else {
        h.push_str(&format!(
            "\nAnswer: <tg-spoiler>{}</tg-spoiler>\n",
            esc(&c.answer)
        ));
    }
    if !c.accepted_answers.is_empty() {
        let alts: Vec<String> = c.accepted_answers.iter().map(|a| esc(a)).collect();
        h.push_str(&format!(
            "Also accepted: <tg-spoiler>{}</tg-spoiler>\n",
            alts.join(", ")
        ));
    }
    if !c.fun_fact.is_empty() {
        h.push_str(&format!(
            "💡 <tg-spoiler>{}</tg-spoiler>\n",
            esc(&c.fun_fact)
        ));
    }
    for m in &d.duplicates {
        let icon = if m.band == Band::LikelyDuplicate {
            "⚠️"
        } else {
            "ℹ️"
        };
        h.push_str(&format!(
            "{icon} {} #{} ({})\n",
            m.band.label(),
            m.question_id,
            m.status
        ));
    }
    h.push_str(&format!("\nby {}\n", esc(&d.author_name)));
    let yes: Vec<String> = d
        .votes
        .iter()
        .filter(|(_, v)| *v == Vote::Yes)
        .map(|(n, _)| esc(n))
        .collect();
    let no: Vec<String> = d
        .votes
        .iter()
        .filter(|(_, v)| *v == Vote::No)
        .map(|(n, _)| esc(n))
        .collect();
    h.push_str(&format!("✅ {}", yes.len()));
    if !yes.is_empty() {
        h.push_str(&format!(" ({})", yes.join(", ")));
    }
    h.push_str(&format!(" · ❌ {}", no.len()));
    if !no.is_empty() {
        h.push_str(&format!(" ({})", no.join(", ")));
    }
    h.push('\n');
    let mut buttons = Vec::new();
    match d.outcome.as_deref() {
        Some("accepted") => h.push_str("\n<b>✅ Accepted</b>"),
        Some("rejected") => h.push_str("\n<b>❌ Rejected</b> (no majority before the deadline)"),
        Some("superseded") => h.push_str("\n<i>↻ Changed since; see the newer card.</i>"),
        _ => {
            let closes = parse_ts(&d.closes_at).format("%Y-%m-%d %H:%M UTC");
            h.push_str(&format!("Voting closes {closes}"));
            buttons.push(vec![
                Button::Callback {
                    text: "✅ Add it".into(),
                    data: format!("v:{}:{}:y", d.question_id, d.round),
                },
                Button::Callback {
                    text: "❌ No".into(),
                    data: format!("v:{}:{}:n", d.question_id, d.round),
                },
            ]);
        }
    }
    if let Some(url) = &d.web_url {
        buttons.push(vec![Button::Url {
            text: "Open".into(),
            url: url.clone(),
        }]);
    }
    OutMessage {
        html: h,
        buttons,
        file: None,
    }
}

pub async fn card_data(app: &App, qid: i64, round: i64) -> Result<CardData> {
    let mut conn = app.db.acquire().await?;
    let q = questions::question(&mut conn, qid).await?;
    let r = round_row(&mut conn, qid, round).await?;
    // A superseded card keeps showing the version it was voted on.
    let superseded: Option<i64> = sqlx::query_scalar(
        "SELECT superseded_version_id FROM vote_round WHERE question_id = ? AND round = ?",
    )
    .bind(qid)
    .bind(round)
    .fetch_one(&mut *conn)
    .await?;
    let version = match superseded {
        Some(v) => questions::version(&mut conn, v).await?,
        None => questions::current_version(&mut conn, &q).await?,
    };
    let author_name: String = sqlx::query_scalar("SELECT display_name FROM member WHERE id = ?")
        .bind(q.author_id)
        .fetch_one(&mut *conn)
        .await?;
    let mut votes = Vec::new();
    for (m, v) in round_votes(&mut conn, qid, round).await? {
        let n: String = sqlx::query_scalar("SELECT display_name FROM member WHERE id = ?")
            .bind(m)
            .fetch_one(&mut *conn)
            .await?;
        votes.push((n, v));
    }
    Ok(CardData {
        question_id: qid,
        round,
        content: version.content(),
        author_name,
        votes,
        closes_at: r.closes_at,
        outcome: r.outcome,
        duplicates: version.duplicates(),
        web_url: app
            .cfg
            .https()
            .then(|| app.cfg.url(&format!("/questions/{qid}"))),
    })
}

pub async fn load_vote_card(app: &App, qid: i64, round: i64) -> Result<OutMessage> {
    Ok(vote_card(&card_data(app, qid, round).await?))
}

/// A question as text, for `/random` and DMs; the answer is spoilered.
pub fn question_text(qid: i64, c: &Content) -> String {
    let mut h = format!(
        "#{qid} · {} · {}\n<b>{}</b>\n",
        esc(&c.theme),
        c.kind.label(),
        esc(&c.prompt)
    );
    if c.kind == Kind::MultipleChoice {
        for (i, ch) in c.choices.iter().enumerate() {
            h.push_str(&format!("{}) {}\n", (b'A' + i as u8) as char, esc(ch)));
        }
    }
    h.push_str(&format!(
        "Answer: <tg-spoiler>{}</tg-spoiler>",
        esc(&c.answer)
    ));
    if !c.fun_fact.is_empty() {
        h.push_str(&format!(
            "\n💡 <tg-spoiler>{}</tg-spoiler>",
            esc(&c.fun_fact)
        ));
    }
    h
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::Part;

    fn data() -> CardData {
        CardData {
            question_id: 7,
            round: 1,
            content: Content::text("Capital of <Finland>?", "Helsinki", "geography", "en"),
            author_name: "Aino".into(),
            votes: vec![("Bo".into(), Vote::Yes), ("Cy".into(), Vote::No)],
            closes_at: "2026-10-28T12:00:00.000000Z".into(),
            outcome: None,
            duplicates: vec![Match {
                question_id: 3,
                status: "accepted".into(),
                band: Band::LikelyDuplicate,
                score: 0.9,
                q_sim: 0.9,
                a_sim: 1.0,
            }],
            web_url: Some("https://q.example/questions/7".into()),
        }
    }

    #[test]
    fn open_card_has_buttons_spoiler_and_escaping() {
        let m = vote_card(&data());
        assert!(m.html.contains("Capital of &lt;Finland&gt;?"));
        assert!(m.html.contains("<tg-spoiler>Helsinki</tg-spoiler>"));
        assert!(m.html.contains("✅ 1 (Bo) · ❌ 1 (Cy)"));
        assert!(m.html.contains("Looks like a duplicate #3"));
        assert!(m.html.contains("Voting closes 2026-10-28 12:00 UTC"));
        assert_eq!(m.buttons[0].len(), 2);
        assert_eq!(
            m.buttons[0][0],
            Button::Callback {
                text: "✅ Add it".into(),
                data: "v:7:1:y".into()
            }
        );
        assert!(matches!(&m.buttons[1][0], Button::Url { .. }));
    }

    #[test]
    fn closed_cards_have_no_vote_buttons() {
        for (o, text) in [
            ("accepted", "Accepted"),
            ("rejected", "Rejected"),
            ("superseded", "Changed since"),
        ] {
            let mut d = data();
            d.outcome = Some(o.into());
            d.web_url = None;
            let m = vote_card(&d);
            assert!(m.html.contains(text), "{o}");
            assert!(m.buttons.is_empty());
        }
    }

    #[test]
    fn kinds_render_their_extras() {
        let mut d = data();
        d.round = 2;
        d.content.kind = Kind::MultipleChoice;
        d.content.choices = vec!["Helsinki".into(), "Turku".into(), "Oulu".into()];
        d.content.accepted_answers = vec!["Hki".into()];
        d.content.fun_fact = "Founded 1550".into();
        let m = vote_card(&d);
        assert!(m.html.contains("(round 2)"));
        assert!(m.html.contains("B) Turku"));
        assert!(m.html.contains("Also accepted"));
        assert!(m.html.contains("💡"));
        d.content.kind = Kind::MultiPart;
        d.content.parts = vec![
            Part {
                prompt: Some("North".into()),
                answer: "Oulu".into(),
                points: 1,
            },
            Part {
                prompt: None,
                answer: "Turku".into(),
                points: 2,
            },
        ];
        let m = vote_card(&d);
        assert!(m.html.contains("North: Oulu (1p); Turku (2p)"));
        let t = question_text(7, &d.content);
        assert!(t.contains("#7") && t.contains("💡"));
    }

    #[test]
    fn reactions_are_escaped_and_stable() {
        let a = reaction(Reaction::Shame, "<Bo>", "Is it?");
        assert_eq!(a, reaction(Reaction::Shame, "<Bo>", "Is it?"));
        assert!(a.contains("&lt;Bo&gt;"));
        let b = reaction(Reaction::Boo, "Bo", "How many?");
        assert!(b.starts_with("Boo!"));
        assert!(b.contains("How many?"));
        assert_eq!(difficulty_stars(2), "★★☆☆☆");
    }
}
