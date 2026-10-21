//! The bot's logic end to end: commands, the /add wizard, vote and proposal
//! buttons. Telegram itself is faked.

mod common;

use common::*;
use quiz_forge::bot::{self, Chat, From, on_callback, on_media, on_text};
use quiz_forge::domain::model::MediaKind;
use quiz_forge::service::telegram::Button;
use quiz_forge::service::{media, questions};

const BOT: &str = "quizbot";

fn user(id: i64, name: &str) -> From {
    From {
        id,
        name: name.into(),
        username: None,
    }
}

async fn say(h: &H, chat: Chat, from: &From, t: &str) -> Vec<(i64, String)> {
    let chat_id = if chat == Chat::Group { GROUP } else { from.id };
    on_text(&h.app, chat, chat_id, from, t, BOT, h.now)
        .await
        .unwrap()
        .into_iter()
        .map(|(c, m)| (c, m.html))
        .collect()
}

async fn press(h: &H, from: &From, data: &str) -> bot::CallbackReply {
    on_callback(&h.app, from, data, h.now).await.unwrap()
}

fn last_text(sends: &[(i64, String)]) -> &str {
    &sends.last().unwrap().1
}

async fn wizard_through(h: &H, u: &From, prompt: &str, answer: &str) -> bot::CallbackReply {
    say(h, Chat::Private, u, "/add").await;
    press(h, u, "w:kind:text").await;
    say(h, Chat::Private, u, prompt).await;
    say(h, Chat::Private, u, answer).await;
    press(h, u, "w:diff:2").await;
    press(h, u, "w:theme:2").await; // themes sorted: art, film & tv, food & drink, geography, …
    press(h, u, "w:review").await;
    press(h, u, "w:submit").await
}

#[tokio::test]
async fn group_commands_and_noise() {
    let h = h().await;
    let a = user(1, "Aino");
    assert!(last_text(&say(&h, Chat::Group, &a, "/help@quizbot").await).contains("/add"));
    assert!(last_text(&say(&h, Chat::Group, &a, "/stats").await).contains("📊"));
    assert!(say(&h, Chat::Group, &a, "just chatting").await.is_empty());
    assert!(say(&h, Chat::Group, &a, "/nonsense").await.is_empty());
    assert!(say(&h, Chat::Other, &a, "/help").await.is_empty());
    for cmd in ["/web", "/mine", "/inbox", "/random", "/cancel"] {
        let r = say(&h, Chat::Group, &a, cmd).await;
        assert!(last_text(&r).contains("private chat"), "{cmd}");
        assert!(last_text(&r).contains("t.me/quizbot"));
    }
    assert!(last_text(&say(&h, Chat::Private, &a, "/what").await).contains("/add"));
    let hi = say(&h, Chat::Private, &a, "/start").await;
    assert!(hi[0].1.starts_with("Hi Aino"));
}

#[tokio::test]
async fn wizard_submits_a_question() {
    let h = h().await;
    let a = user(1, "Aino");
    let r = say(&h, Chat::Group, &a, "/add").await;
    assert_eq!(r.len(), 2);
    assert_eq!(r[0].0, GROUP);
    assert!(r[0].1.contains("private message"));
    assert_eq!(r[1].0, 1);
    assert!(r[1].1.contains("What kind"));
    // text while no wizard? there is one now
    let r = press(&h, &a, "w:kind:text").await;
    assert!(r.sends[0].1.html.contains("Send the question"));
    say(&h, Chat::Private, &a, "Mikä on Suomen pääkaupunki?").await;
    say(&h, Chat::Private, &a, "Helsinki").await;
    press(&h, &a, "w:diff:2").await;
    let themes = questions::themes(&h.app).await.unwrap();
    let geo = themes.iter().position(|t| t == "geography").unwrap();
    press(&h, &a, &format!("w:theme:{geo}")).await;
    let r = press(&h, &a, "w:review").await;
    assert!(r.sends[0].1.html.contains("Answer: Helsinki"));
    let r = press(&h, &a, "w:submit").await;
    assert!(r.sends[0].1.html.contains("Submitted as #1"));
    assert_eq!(h.status(1).await, "voting");
    assert_eq!(h.count("SELECT count(*) FROM wizard_state").await, 0);
    // the wizard is gone: buttons from it expire
    assert!(press(&h, &a, "w:submit").await.toast.contains("expired"));
    assert!(last_text(&say(&h, Chat::Private, &a, "hello?").await).contains("/add"));
    // /start add opens a wizard too
    let r = say(&h, Chat::Private, &a, "/start add").await;
    assert!(r[0].1.contains("What kind"));
    assert_eq!(
        last_text(&say(&h, Chat::Private, &a, "/cancel").await),
        "Cancelled."
    );
}

#[tokio::test]
async fn wizard_refusal_duplicates_and_cancel() {
    let h = h().await;
    let a = user(1, "Aino");
    let r = wizard_through(&h, &a, "Onko Helsinki pääkaupunki?", "Kyllä").await;
    assert!(r.sends[0].1.html.contains("⛔ Yes/no questions"));
    assert!(
        r.sends[0].1.html.contains("Review"),
        "back on the review screen"
    );
    // fix the answer from the review screen
    press(&h, &a, "w:edit:prompt").await;
    say(&h, Chat::Private, &a, "Mikä on Suomen pääkaupunki?").await;
    press(&h, &a, "w:edit:answer").await;
    say(&h, Chat::Private, &a, "Helsinki").await;
    assert!(
        press(&h, &a, "w:submit").await.sends[0]
            .1
            .html
            .contains("Submitted")
    );
    h.deliver().await;
    assert!(
        h.group_texts()
            .iter()
            .any(|t| t.to_lowercase().contains("yes/no"))
    );

    // the same question again: duplicate warning, then confirm
    let r = wizard_through(&h, &a, "Mikä on Suomen pääkaupunki?", "Helsinki").await;
    assert!(
        r.sends[0]
            .1
            .html
            .contains("Similar questions already exist")
    );
    let r = press(&h, &a, "w:dup_yes").await;
    assert!(r.sends[0].1.html.contains("Submitted as #2"));

    // cancel mid-way
    say(&h, Chat::Private, &a, "/add").await;
    let r = press(&h, &a, "w:cancel").await;
    assert!(r.sends[0].1.html.contains("Cancelled"));
    assert_eq!(h.count("SELECT count(*) FROM wizard_state").await, 0);
}

#[tokio::test]
async fn wizard_with_pictures() {
    let h = h().await;
    let a = user(1, "Aino");
    say(&h, Chat::Private, &a, "/add").await;
    press(&h, &a, "w:kind:image").await;
    say(&h, Chat::Private, &a, "Kuka on kuvassa?").await;
    say(&h, Chat::Private, &a, "Sibelius").await;
    let m = media::store(&h.app, PNG, 1, h.now).await.unwrap();
    let r = on_media(&h.app, &a, m.id, MediaKind::Image, h.now)
        .await
        .unwrap();
    assert!(r[0].1.html.contains("Got 1"));
    press(&h, &a, "w:media_done").await;
    press(&h, &a, "w:diff:3").await;
    let themes = questions::themes(&h.app).await.unwrap();
    let music = themes.iter().position(|t| t == "music").unwrap();
    press(&h, &a, &format!("w:theme:{music}")).await;
    press(&h, &a, "w:review").await;
    // this one question is in English: switch from the review screen
    press(&h, &a, "w:edit:language").await;
    let back = say(&h, Chat::Private, &a, "en").await;
    assert!(last_text(&back).contains("Review"));
    assert!(
        press(&h, &a, "w:submit").await.sends[0]
            .1
            .html
            .contains("Submitted")
    );
    let d = questions::detail(&h.app, 1).await.unwrap();
    assert_eq!(d.current.content.media.len(), 1);
    assert_eq!(d.current.content.language, "en");
}

#[tokio::test]
async fn votes_proposals_and_private_commands() {
    let h = h().await;
    let a = user(1, "Aino");
    let b = user(2, "Bo");
    let aino = h.member(1, "Aino").await;
    let qid = h.submitted(&aino, text("Pisin joki?", "Niili")).await;

    assert!(
        press(&h, &a, &format!("v:{qid}:1:y"))
            .await
            .toast
            .contains("own question")
    );
    assert_eq!(
        press(&h, &b, "v:x:1:y").await.toast,
        "That button is broken."
    );
    assert_eq!(
        press(&h, &b, "v:1:1:maybe").await.toast,
        "That button is broken."
    );
    assert_eq!(press(&h, &b, "zzz").await.toast, "Unknown button.");
    let c = user(3, "Cy");
    assert_eq!(
        press(&h, &c, &format!("v:{qid}:1:n")).await.toast,
        "Vote recorded: ✅ 0 · ❌ 1"
    );
    assert!(
        press(&h, &b, &format!("v:{qid}:1:y"))
            .await
            .toast
            .contains("Vote recorded")
    );
    let d = user(4, "Di");
    assert!(
        press(&h, &d, &format!("v:{qid}:1:y"))
            .await
            .toast
            .contains("Accepted!")
    );

    // a proposal from Bo shows up in Aino's /inbox with buttons
    let bo = h.member(2, "Bo").await;
    let mut c2 = text("Pisin joki?", "Niili");
    c2.fun_fact = "6650 km".into();
    questions::edit(&h.app, qid, &bo, c2, "", h.now)
        .await
        .unwrap();
    let inbox = on_text(&h.app, Chat::Private, 1, &a, "/inbox", BOT, h.now)
        .await
        .unwrap();
    assert_eq!(inbox.len(), 1);
    assert_eq!(
        inbox[0].1.buttons[0][0],
        Button::Callback {
            text: "✅ Accept".into(),
            data: "p:1:a".into()
        }
    );
    assert!(last_text(&say(&h, Chat::Private, &b, "/inbox").await).contains("No proposals"));
    assert_eq!(
        press(&h, &b, "p:1:a").await.toast,
        "you can't approve your own proposal"
    );
    assert_eq!(press(&h, &a, "p:x:a").await.toast, "That button is broken.");
    assert_eq!(press(&h, &a, "p:1:a").await.toast, "Accepted.");
    assert!(
        press(&h, &a, "p:1:d")
            .await
            .toast
            .contains("already accepted")
    );

    let mine = say(&h, Chat::Private, &a, "/mine").await;
    assert!(last_text(&mine).contains("#1 accepted"));
    assert!(last_text(&say(&h, Chat::Private, &b, "/mine").await).contains("haven't submitted"));
    let web = say(&h, Chat::Private, &a, "/web").await;
    assert!(last_text(&web).contains("/login/"));
    let rnd = say(&h, Chat::Private, &b, "/random").await;
    assert!(last_text(&rnd).contains("Pisin joki?"));
    assert!(
        last_text(&say(&h, Chat::Private, &b, "/random music").await)
            .contains("No accepted questions")
    );
    let stats = say(&h, Chat::Private, &b, "/stats").await;
    assert!(last_text(&stats).contains("accepted: 1"));
}

#[tokio::test]
async fn outsiders_are_turned_away() {
    let h = h().await;
    h.tg.non_members.lock().unwrap().push(9);
    let x = user(9, "Stranger");
    for cmd in [
        "/add", "/mine", "/inbox", "/web", "/random", "/start", "hello",
    ] {
        let r = say(&h, Chat::Private, &x, cmd).await;
        assert!(last_text(&r).contains("not in it"), "{cmd}");
    }
    assert!(
        press(&h, &x, "v:1:1:y")
            .await
            .toast
            .contains("Only members")
    );
    let r = on_media(&h.app, &x, 1, MediaKind::Image, h.now)
        .await
        .unwrap();
    assert!(r[0].1.html.contains("not in it"));
}

#[tokio::test]
async fn group_language_setting() {
    let h = h().await;
    let a = user(1, "Aino");
    // the instance default until someone sets one
    assert!(last_text(&say(&h, Chat::Group, &a, "/language").await).contains("<b>fi</b>"));
    let r = say(&h, Chat::Group, &a, "/language Not A Code").await;
    assert!(last_text(&r).contains("short language code"));
    let r = say(&h, Chat::Group, &a, "/language EN").await;
    assert_eq!(r[0].0, GROUP);
    assert!(last_text(&r).contains("Aino set the group's question language to <b>en</b>"));
    assert!(last_text(&say(&h, Chat::Private, &a, "/language").await).contains("<b>en</b>"));

    // the wizard never asks, and the question gets the group's language
    say(&h, Chat::Private, &a, "/add").await;
    press(&h, &a, "w:kind:text").await;
    say(&h, Chat::Private, &a, "What is the capital of Finland?").await;
    say(&h, Chat::Private, &a, "Helsinki").await;
    press(&h, &a, "w:diff:2").await;
    let r = press(&h, &a, "w:theme:0").await;
    assert!(
        r.sends[0].1.html.contains("Anything else?"),
        "straight to the extras"
    );
    let r = press(&h, &a, "w:review").await;
    assert!(r.sends[0].1.html.contains(" · en"));
    assert!(
        press(&h, &a, "w:submit").await.sends[0]
            .1
            .html
            .contains("Submitted")
    );
    let d = questions::detail(&h.app, 1).await.unwrap();
    assert_eq!(d.current.content.language, "en");

    // outsiders can look but not change it
    h.tg.non_members.lock().unwrap().push(9);
    let x = user(9, "Stranger");
    assert!(last_text(&say(&h, Chat::Private, &x, "/language sv").await).contains("not in it"));
    assert!(last_text(&say(&h, Chat::Private, &a, "/language").await).contains("<b>en</b>"));
    assert!(bot::HELP.contains("/language"));
}
