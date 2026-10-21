//! Quizzes, events, auth, membership, outbox delivery and review recording.

mod common;

use chrono::Duration;
use common::*;
use quiz_forge::domain::model::Kind;
use quiz_forge::service::outbox::{Outgoing, enqueue};
use quiz_forge::service::reviews::{self, ReviewResult, TextFixes, Verdict};
use quiz_forge::service::telegram::{OutMessage, TgError};
use quiz_forge::service::{AppError, auth, members, questions, quizzes};

const TPL: &str = r#"
name = "Night"
language = "fi"
[[round]]
title = "Warm-up"
[[round.slot]]
count = 3
[[round]]
title = "Pictures"
[[round.slot]]
count = 2
kind = "image"
theme = "$a"
[[round]]
title = "Tiebreaker"
[[round.slot]]
kind = "tiebreaker"
"#;

/// A bank with 8 text questions, 4 image questions in "music" and a tiebreaker.
async fn bank(
    h: &H,
) -> (
    quiz_forge::service::members::Member,
    quiz_forge::service::members::Member,
) {
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    let rivers = [
        "Niili",
        "Amazon",
        "Jangtse",
        "Mississippi",
        "Jenisei",
        "Kongo",
        "Amur",
        "Lena",
    ];
    for (i, r) in rivers.iter().enumerate() {
        h.accepted(
            &a,
            &b,
            themed(
                &format!("Kysymys {i}: mikä joki virtaa paikassa {}{}?", r, i * 7),
                r,
                "geography",
                Kind::Text,
                2,
            ),
        )
        .await;
    }
    for i in 0..4u8 {
        let mut png = PNG.to_vec();
        png.push(i);
        let m = quiz_forge::service::media::store(&h.app, &png, a.id, h.now)
            .await
            .unwrap();
        h.accepted(
            &a,
            &b,
            image_q(
                &format!("Kuka säveltäjä on kuvassa {i}?"),
                &format!("Säveltäjä{i}"),
                "music",
                m.id,
            ),
        )
        .await;
    }
    let mut tb = themed(
        "Kuinka monta saarta Suomessa on?",
        "178947",
        "geography",
        Kind::Tiebreaker,
        3,
    );
    tb.kind = Kind::Tiebreaker;
    h.accepted(&a, &b, tb).await;
    (a, b)
}

#[tokio::test]
async fn templates_quizzes_and_events() {
    let mut h = h().await;
    let (a, b) = bank(&h).await;
    assert!(
        quizzes::save_template(&h.app, None, "name = ", &a, h.now)
            .await
            .is_err()
    );
    let tid = quizzes::save_template(&h.app, None, TPL, &a, h.now)
        .await
        .unwrap();
    assert_eq!(quizzes::templates(&h.app).await.unwrap()[0].name, "Night");
    assert_eq!(
        quizzes::save_template(&h.app, Some(tid), TPL, &a, h.now)
            .await
            .unwrap(),
        tid
    );
    assert!(matches!(
        quizzes::save_template(&h.app, Some(999), TPL, &a, h.now).await,
        Err(AppError::NotFound)
    ));

    let p_bo = quizzes::add_player(&h.app, "Bo", Some(b.id), h.now)
        .await
        .unwrap();
    let p_guest = quizzes::add_player(&h.app, "Guest", None, h.now)
        .await
        .unwrap();
    assert!(matches!(
        quizzes::add_player(&h.app, "Bo", None, h.now).await,
        Err(AppError::Conflict(_))
    ));
    assert!(quizzes::add_player(&h.app, "", None, h.now).await.is_err());
    assert_eq!(
        quizzes::import_members_as_players(&h.app, h.now)
            .await
            .unwrap(),
        1,
        "Aino gets a player row"
    );

    // Bo has voted on everything, so with Bo playing everything is spoiled
    let spoiled_quiz = quizzes::create_quiz(&h.app, tid, "", Some("s1"), &[p_bo], &a, h.now)
        .await
        .unwrap();
    let v = quizzes::view(&h.app, spoiled_quiz).await.unwrap();
    assert!(
        v.rounds
            .iter()
            .flat_map(|r| &r.slots)
            .all(|s| s.question_id.is_none())
    );
    quizzes::delete_quiz(&h.app, spoiled_quiz).await.unwrap();

    let qz = quizzes::create_quiz(&h.app, tid, "October", Some("s1"), &[p_guest], &a, h.now)
        .await
        .unwrap();
    let v = quizzes::view(&h.app, qz).await.unwrap();
    assert_eq!(v.quiz.name, "October");
    assert_eq!(v.rounds.len(), 3);
    assert!(
        v.rounds
            .iter()
            .flat_map(|r| &r.slots)
            .all(|s| s.question_id.is_some()),
        "{:?}",
        v.rounds
    );
    assert_eq!(
        v.rounds[1].slots[0].content.as_ref().unwrap().kind,
        Kind::Image
    );
    assert_eq!(
        v.rounds[2].slots[0].content.as_ref().unwrap().kind,
        Kind::Tiebreaker
    );
    assert_eq!(v.quiz.bindings()["r2.$a"], "music");
    // same seed, same quiz
    let again = quizzes::create_quiz(&h.app, tid, "", Some("s1"), &[p_guest], &a, h.now)
        .await
        .unwrap();
    assert_eq!(
        quizzes::view(&h.app, again).await.unwrap().question_ids(),
        v.question_ids()
    );

    // swap: options exclude what's in the quiz; picture slot keeps its theme
    let opts = quizzes::swap_options(&h.app, qz, (0, 0), 10).await.unwrap();
    assert_eq!(opts.len(), 7, "12 non-tiebreakers minus the 5 in the quiz");
    let pics = quizzes::swap_options(&h.app, qz, (1, 0), 10).await.unwrap();
    assert!(
        pics.iter()
            .all(|(_, c)| c.kind == Kind::Image && c.theme == "music")
    );
    let (swap_in, _) = opts[0].clone();
    quizzes::set_slot(&h.app, qz, (0, 0), swap_in)
        .await
        .unwrap();
    let in_quiz = v.rounds[0].slots[1].question_id.unwrap();
    assert!(matches!(
        quizzes::set_slot(&h.app, qz, (0, 0), in_quiz).await,
        Err(AppError::Conflict(_))
    ));
    assert!(quizzes::set_slot(&h.app, qz, (0, 0), 9999).await.is_err());

    // refill keeps the locked slot
    quizzes::refill(&h.app, qz, Some("s2"), h.now)
        .await
        .unwrap();
    let v2 = quizzes::view(&h.app, qz).await.unwrap();
    assert_eq!(v2.rounds[0].slots[0].question_id, Some(swap_in));
    assert!(v2.rounds[0].slots[0].locked);
    assert_eq!(v2.quiz.seed, "s2");
    quizzes::set_locked(&h.app, qz, (0, 0), false)
        .await
        .unwrap();
    quizzes::clear_slot(&h.app, qz, (0, 1)).await.unwrap();
    assert!(matches!(
        quizzes::set_locked(&h.app, qz, (0, 1), true).await,
        Err(AppError::Invalid(_))
    ));
    assert!(
        matches!(
            quizzes::freeze(&h.app, qz, h.now).await,
            Err(AppError::Conflict(_))
        ),
        "empty slot"
    );
    assert!(matches!(
        quizzes::create_event(&h.app, qz, "2026-10-31", "", &[], h.now).await,
        Err(AppError::Conflict(_))
    ));
    quizzes::refill(&h.app, qz, None, h.now).await.unwrap();
    quizzes::set_players(&h.app, qz, &[p_guest]).await.unwrap();
    quizzes::freeze(&h.app, qz, h.now).await.unwrap();
    assert!(
        quizzes::refill(&h.app, qz, None, h.now).await.is_err(),
        "frozen"
    );
    assert!(
        quizzes::set_slot(&h.app, qz, (0, 0), swap_in)
            .await
            .is_err()
    );

    // an edit after freezing shows as "newer available"
    let frozen = quizzes::view(&h.app, qz).await.unwrap();
    let first = &frozen.rounds[0].slots[0];
    let qid = first.question_id.unwrap();
    let mut c = first.content.clone().unwrap();
    c.fun_fact = "new".into();
    questions::edit(&h.app, qid, &a, c, "", h.now)
        .await
        .unwrap();
    let after = quizzes::view(&h.app, qz).await.unwrap();
    assert!(after.rounds[0].slots[0].newer_available);
    assert_eq!(
        after.rounds[0].slots[0].content.as_ref().unwrap().fun_fact,
        "",
        "still shows the pinned version"
    );
    quizzes::take_newer(&h.app, qz, (0, 0)).await.unwrap();
    assert!(!quizzes::view(&h.app, qz).await.unwrap().rounds[0].slots[0].newer_available);

    // events
    assert!(
        quizzes::create_event(&h.app, qz, "31.10.2026", "", &[], h.now)
            .await
            .is_err()
    );
    let ev = quizzes::create_event(&h.app, qz, "2026-10-31", "", &[p_guest, p_bo], h.now)
        .await
        .unwrap();
    assert_eq!(quizzes::events(&h.app).await.unwrap()[0].title, "October");
    assert_eq!(quizzes::event_players(&h.app, ev).await.unwrap().len(), 2);
    h.tick(Duration::days(30));
    quizzes::mark_held(&h.app, ev, &a, h.now).await.unwrap();
    assert!(quizzes::mark_held(&h.app, ev, &a, h.now).await.is_err());
    assert_eq!(h.count("SELECT count(*) FROM question_usage").await, 6);
    assert!(matches!(
        quizzes::unfreeze(&h.app, qz).await,
        Err(AppError::Conflict(_))
    ));
    assert!(matches!(
        quizzes::delete_quiz(&h.app, qz).await,
        Err(AppError::Conflict(_))
    ));
    let d = questions::detail(&h.app, qid).await.unwrap();
    assert_eq!(d.used_at.len(), 1);

    // used questions stay out of the next quiz; the guest has now heard them too
    let next = quizzes::create_quiz(&h.app, tid, "", Some("s1"), &[], &a, h.now)
        .await
        .unwrap();
    let nv = quizzes::view(&h.app, next).await.unwrap();
    assert!(nv.question_ids().is_disjoint(&after.question_ids()));
    let mut conn = h.app.db.acquire().await.unwrap();
    let spoiled = quizzes::spoiled_for(&mut conn, &[p_guest]).await.unwrap();
    assert_eq!(spoiled.len(), 6);
    drop(conn);
    quizzes::unfreeze(&h.app, next).await.unwrap();
    quizzes::delete_template(&h.app, tid).await.unwrap();
    assert!(
        quizzes::quizzes(&h.app)
            .await
            .unwrap()
            .iter()
            .all(|q| q.template_id.is_none())
    );
    assert_eq!(quizzes::players(&h.app).await.unwrap().len(), 3);
}

#[tokio::test]
async fn login_links_and_sessions() {
    let mut h = h().await;
    let a = h.member(1, "Aino").await;
    let token = auth::create_login_token(&h.app, &a, h.now).await.unwrap();
    assert!(auth::login_url(&h.app, &token).ends_with(&format!("/login/{token}")));
    assert!(auth::redeem(&h.app, "nope", h.now).await.is_err());
    let s = auth::redeem(&h.app, &token, h.now).await.unwrap();
    assert_eq!(s.member_id, a.id);
    assert!(
        auth::redeem(&h.app, &token, h.now).await.is_err(),
        "single use"
    );
    let sess = auth::session(&h.app, &s.raw_id, h.now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(sess.member.id, a.id);
    assert_eq!(sess.csrf.len(), 64);
    assert!(
        auth::session(&h.app, "bogus", h.now)
            .await
            .unwrap()
            .is_none()
    );

    // expired link
    let t2 = auth::create_login_token(&h.app, &a, h.now).await.unwrap();
    h.tick(Duration::minutes(11));
    assert!(auth::redeem(&h.app, &t2, h.now).await.is_err());

    // daily membership re-check ends the session of someone who left
    h.tick(Duration::hours(25));
    h.tg.non_members.lock().unwrap().push(1);
    assert!(
        auth::session(&h.app, &s.raw_id, h.now)
            .await
            .unwrap()
            .is_none()
    );

    // sessions expire, logout works
    h.tg.non_members.lock().unwrap().clear();
    let t3 = auth::create_login_token(&h.app, &a, h.now).await.unwrap();
    let s3 = auth::redeem(&h.app, &t3, h.now).await.unwrap();
    h.tick(Duration::days(31));
    assert!(
        auth::session(&h.app, &s3.raw_id, h.now)
            .await
            .unwrap()
            .is_none()
    );
    let t4 = auth::create_login_token(&h.app, &a, h.now).await.unwrap();
    let s4 = auth::redeem(&h.app, &t4, h.now).await.unwrap();
    auth::logout(&h.app, &s4.raw_id).await.unwrap();
    assert!(
        auth::session(&h.app, &s4.raw_id, h.now)
            .await
            .unwrap()
            .is_none()
    );
}

#[tokio::test]
async fn membership_is_cached_and_survives_telegram_errors() {
    let mut h = h().await;
    let a = h.member(1, "Aino").await;
    assert!(members::ensure_in_group(&h.app, &a, h.now).await.unwrap());
    h.tg.non_members.lock().unwrap().push(1);
    let cached = members::get(&h.app, a.id).await.unwrap();
    assert!(
        members::ensure_in_group(&h.app, &cached, h.now)
            .await
            .unwrap(),
        "cached for ten minutes"
    );
    h.tick(Duration::minutes(11));
    assert!(
        !members::ensure_in_group(&h.app, &cached, h.now)
            .await
            .unwrap()
    );
    let out = members::get(&h.app, a.id).await.unwrap();
    assert!(
        members::require_in_group(&h.app, &out, h.now)
            .await
            .is_err()
    );
    let agent = members::agent(&h.app, h.now).await.unwrap();
    assert!(agent.is_agent());
    assert_eq!(members::agent(&h.app, h.now).await.unwrap().id, agent.id);
    assert!(
        members::ensure_in_group(&h.app, &agent, h.now)
            .await
            .unwrap()
    );
    assert_eq!(members::list(&h.app).await.unwrap().len(), 2);
    assert!(members::by_telegram(&h.app, 42).await.unwrap().is_none());
    // renames follow Telegram
    let renamed = members::upsert_telegram(&h.app, 1, "Aino K", Some("ainok"), h.now)
        .await
        .unwrap();
    assert_eq!(
        (renamed.id, renamed.display_name.as_str()),
        (a.id, "Aino K")
    );
    let blank = members::upsert_telegram(&h.app, 5, " ", Some("user5"), h.now)
        .await
        .unwrap();
    assert_eq!(blank.display_name, "user5");
}

#[tokio::test]
async fn outbox_retries_fails_and_dedupes() {
    let mut h = h().await;
    let a = h.member(1, "Aino").await;
    let agent = members::agent(&h.app, h.now).await.unwrap();
    let mut conn = h.app.db.acquire().await.unwrap();
    let g = Outgoing::Group {
        msg: OutMessage::text("hello"),
    };
    enqueue(&mut conn, &g, Some("k"), h.now).await.unwrap();
    enqueue(&mut conn, &g, Some("k"), h.now).await.unwrap();
    enqueue(
        &mut conn,
        &Outgoing::Direct {
            member_id: agent.id,
            msg: OutMessage::text("x"),
        },
        None,
        h.now,
    )
    .await
    .unwrap();
    enqueue(
        &mut conn,
        &Outgoing::Direct {
            member_id: a.id,
            msg: OutMessage::text("dm"),
        },
        None,
        h.now,
    )
    .await
    .unwrap();
    enqueue(
        &mut conn,
        &Outgoing::VoteCardRefresh {
            question_id: 1,
            round: 1,
        },
        None,
        h.now,
    )
    .await
    .unwrap();
    drop(conn);
    // first send fails transiently, the DM permanently
    h.tg.fail_next.lock().unwrap().extend([
        TgError::Permanent("blocked".into()),
        TgError::Transient("timeout".into()),
    ]);
    assert_eq!(
        h.deliver().await,
        2,
        "agent DM (skipped) and refresh of an unposted card"
    );
    assert_eq!(
        h.count("SELECT count(*) FROM outbox WHERE failed_at IS NOT NULL")
            .await,
        1
    );
    assert_eq!(h.deliver().await, 0, "group message waits for its backoff");
    h.tick(Duration::seconds(31));
    assert_eq!(h.deliver().await, 1);
    assert_eq!(h.group_texts(), vec!["hello".to_string()]);
    assert_eq!(
        h.count("SELECT count(*) FROM outbox WHERE sent_at IS NULL AND failed_at IS NULL")
            .await,
        0
    );

    let mut conn = h.app.db.acquire().await.unwrap();
    enqueue(
        &mut conn,
        &Outgoing::Group {
            msg: OutMessage::text("later"),
        },
        None,
        h.now,
    )
    .await
    .unwrap();
    drop(conn);
    h.tg.fail_next.lock().unwrap().push(TgError::RetryAfter(5));
    assert_eq!(h.deliver().await, 0);
    h.tick(Duration::seconds(5));
    assert_eq!(h.deliver().await, 1);
}

fn review(verdict: Verdict) -> ReviewResult {
    ReviewResult {
        verdict,
        explanation: "Checked against an atlas.".into(),
        sources: vec!["https://example.org/rivers".into()],
        typo_fixes: None,
        corrected_answer: None,
        fun_fact: None,
        background: None,
        difficulty: None,
        tags: None,
        flags: vec![],
        duplicate_of: vec![],
    }
}

#[tokio::test]
async fn reviews_auto_apply_typos_and_propose_the_rest() {
    let h = h().await;
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    let qid = h
        .accepted(&a, &b, text("Mikä on maailman pisn joki?", "Niili"))
        .await;
    let other = h.accepted(&a, &b, text("Korkein vuori?", "Everest")).await;
    assert_eq!(
        reviews::pending(&h.app, 10).await.unwrap(),
        vec![qid, other]
    );
    let run = reviews::start_run(&h.app, "fake", "m", h.now)
        .await
        .unwrap();
    let (vid, c, neighbours) = reviews::context(&h.app, qid, 5).await.unwrap();
    assert_eq!(c.prompt, "Mikä on maailman pisn joki?");
    assert_eq!(neighbours, vec![(other, "Korkein vuori?".to_string())]);

    let mut r = review(Verdict::Ok);
    r.typo_fixes = Some(TextFixes {
        prompt: Some("Mikä on maailman pisin joki?".into()),
        ..Default::default()
    });
    r.fun_fact = Some("It flows through eleven countries.".into());
    r.duplicate_of = vec![other, qid, 999];
    let out = reviews::record(&h.app, run, qid, vid, &r, h.now)
        .await
        .unwrap();
    assert!(out.auto_applied.is_some());
    assert!(out.proposal.is_some());
    assert_eq!(out.typo_rejected, None);
    let d = questions::detail(&h.app, qid).await.unwrap();
    assert_eq!(d.current.content.prompt, "Mikä on maailman pisin joki?");
    assert_eq!(d.current.by_name, "Review agent");
    assert_eq!(d.q.status, "accepted", "typo fix doesn't reopen voting");
    assert_eq!(d.q.last_reviewed_version_id, d.q.current_version_id);
    assert_eq!(d.q.review_flag, None);
    assert_eq!(
        d.proposals[0].changes[0].0,
        quiz_forge::domain::model::Field::FunFact
    );
    assert_eq!(
        d.notes.len(),
        1,
        "one dup note; self and unknown ids ignored"
    );
    assert_eq!(d.reviews.len(), 1);
    assert_eq!(reviews::pending(&h.app, 10).await.unwrap(), vec![other]);
    h.deliver().await;
    assert!(
        h.dm_texts(1).iter().any(|t| t.contains("fixed typos")
            && t.contains("pisn joki? → Mikä on maailman pisin joki?"))
    );
    reviews::finish_run(&h.app, run, 1, 100, 50, 0.01, None, h.now)
        .await
        .unwrap();
}

#[tokio::test]
async fn reviews_refuse_big_typo_fixes_and_flag_wrong_answers() {
    let h = h().await;
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    let qid = h.accepted(&a, &b, text("Pisin joki?", "Amazon")).await;
    let run = reviews::start_run(&h.app, "fake", "m", h.now)
        .await
        .unwrap();
    let (vid, _, _) = reviews::context(&h.app, qid, 5).await.unwrap();
    let mut r = review(Verdict::Wrong);
    r.typo_fixes = Some(TextFixes {
        prompt: Some("Mikä on maailman pisin joki?".into()),
        ..Default::default()
    });
    r.corrected_answer = Some("Niili".into());
    r.flags = vec!["contested".into()];
    let out = reviews::record(&h.app, run, qid, vid, &r, h.now)
        .await
        .unwrap();
    assert_eq!(out.auto_applied, None);
    assert!(out.typo_rejected.unwrap().contains("words were added"));
    let pid = out.proposal.unwrap();
    let d = questions::detail(&h.app, qid).await.unwrap();
    assert_eq!(d.q.review_flag.as_deref(), Some("wrong"));
    let p = d.proposals.iter().find(|p| p.row.id == pid).unwrap();
    assert!(p.row.material);
    assert!(p.row.rationale.starts_with("Review agent (wrong)"));
    h.deliver().await;
    assert!(h.dm_texts(1).iter().any(|t| t.contains("may be wrong")));
    // the author accepts the agent's correction: it's material on an accepted
    // question, but proposer (agent) + author make two
    questions::decide(&h.app, pid, &a, true, h.now)
        .await
        .unwrap();
    let d = questions::detail(&h.app, qid).await.unwrap();
    assert_eq!(d.current.content.answer, "Niili");
    assert_eq!(d.current.content.prompt, "Mikä on maailman pisin joki?");
}

#[tokio::test]
async fn review_of_a_moved_on_question_is_only_recorded() {
    let h = h().await;
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    let qid = h.accepted(&a, &b, text("Pisin joki?", "Niili")).await;
    let run = reviews::start_run(&h.app, "fake", "m", h.now)
        .await
        .unwrap();
    let (vid, _, _) = reviews::context(&h.app, qid, 5).await.unwrap();
    let mut c = text("Pisin joki?", "Niili");
    c.tags = vec!["africa".into()];
    questions::edit(&h.app, qid, &a, c, "", h.now)
        .await
        .unwrap();
    let mut r = review(Verdict::Doubtful);
    r.fun_fact = Some("x".into());
    let out = reviews::record(&h.app, run, qid, vid, &r, h.now)
        .await
        .unwrap();
    assert_eq!((out.auto_applied, out.proposal), (None, None));
    let d = questions::detail(&h.app, qid).await.unwrap();
    assert_eq!(d.q.review_flag, None);
    assert_eq!(
        reviews::pending(&h.app, 10).await.unwrap(),
        vec![qid],
        "the new version still needs a review"
    );
    assert_eq!(Verdict::TimeSensitive.as_str(), "time_sensitive");
    assert_eq!(Verdict::Unverifiable.as_str(), "unverifiable");
}
