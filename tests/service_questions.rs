//! Submitting, voting, editing and proposals, end to end through the service
//! layer, with the outbox delivered to a fake Telegram.

mod common;

use chrono::Duration;
use common::*;
use quiz_forge::domain::model::{Field, Kind};
use quiz_forge::domain::rules::RuleError;
use quiz_forge::domain::voting::{Outcome, Vote};
use quiz_forge::service::questions::{self, DecideOutcome, EditOutcome, Filter, SubmitOutcome};
use quiz_forge::service::telegram::Button;
use quiz_forge::service::{AppError, media, members};

#[tokio::test]
async fn submit_vote_accept_flow() {
    let h = h().await;
    let aino = h.member(1, "Aino").await;
    let bo = h.member(2, "Bo").await;
    let qid = h
        .submitted(&aino, text("Mikä on Suomen pääkaupunki?", "Helsinki"))
        .await;
    assert_eq!(h.status(qid).await, "voting");

    assert_eq!(h.deliver().await, 1);
    let card = &h.tg.sent.lock().unwrap()[0].1.clone();
    assert!(card.html.contains("Question #1"));
    assert!(card.html.contains("<tg-spoiler>Helsinki</tg-spoiler>"));
    assert_eq!(
        card.buttons[0][0],
        Button::Callback {
            text: "✅ Add it".into(),
            data: format!("v:{qid}:1:y")
        }
    );

    let err = questions::vote(&h.app, qid, Some(1), &aino, Vote::Yes, h.now)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("own question"));

    let out = questions::vote(&h.app, qid, Some(1), &bo, Vote::Yes, h.now)
        .await
        .unwrap();
    assert_eq!(out.outcome, Some(Outcome::Accepted));
    assert_eq!((out.yes, out.no), (1, 0));
    assert_eq!(h.status(qid).await, "accepted");

    h.deliver().await;
    let edited = h.tg.edited.lock().unwrap().clone();
    assert_eq!(edited.len(), 1);
    assert_eq!(edited[0].1, 1, "edits the posted card");
    assert!(edited[0].2.html.contains("✅ Accepted"));
    assert!(edited[0].2.buttons.is_empty());
    assert!(h.dm_texts(1).iter().any(|t| t.contains("was accepted")));

    // closed: no more votes
    let err = questions::vote(&h.app, qid, None, &bo, Vote::No, h.now)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Conflict(_)));
    // exposures: author and voter
    assert_eq!(h.count("SELECT count(*) FROM exposure").await, 2);
}

#[tokio::test]
async fn a_no_vote_needs_two_yes() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let b = h.member(2, "B").await;
    let c = h.member(3, "C").await;
    let d = h.member(4, "D").await;
    let qid = h.submitted(&a, text("Pisin joki?", "Niili")).await;
    questions::vote(&h.app, qid, None, &b, Vote::No, h.now)
        .await
        .unwrap();
    let r = questions::vote(&h.app, qid, None, &c, Vote::Yes, h.now)
        .await
        .unwrap();
    assert_eq!(r.outcome, None);
    let r = questions::vote(&h.app, qid, None, &d, Vote::Yes, h.now)
        .await
        .unwrap();
    assert_eq!(r.outcome, Some(Outcome::Accepted));
}

#[tokio::test]
async fn expiry_rejects_once() {
    let mut h = h().await;
    let a = h.member(1, "A").await;
    let b = h.member(2, "B").await;
    let qid = h.submitted(&a, text("Pisin joki?", "Niili")).await;
    questions::vote(&h.app, qid, None, &b, Vote::No, h.now)
        .await
        .unwrap();
    h.deliver().await;
    assert_eq!(questions::expire_votes(&h.app, h.now).await.unwrap(), 0);
    h.tick(Duration::days(7));
    let err = questions::vote(&h.app, qid, None, &b, Vote::Yes, h.now)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("run out"));
    assert_eq!(questions::expire_votes(&h.app, h.now).await.unwrap(), 1);
    assert_eq!(questions::expire_votes(&h.app, h.now).await.unwrap(), 0);
    assert_eq!(h.status(qid).await, "rejected");
    h.deliver().await;
    assert!(
        h.tg.edited
            .lock()
            .unwrap()
            .last()
            .unwrap()
            .2
            .html
            .contains("Rejected")
    );
    assert!(h.dm_texts(1).iter().any(|t| t.contains("rejected")));
}

#[tokio::test]
async fn yes_no_is_refused_and_shamed_in_group_once() {
    let h = h().await;
    let a = h.member(1, "Aino").await;
    let c = text("Onko Helsinki Suomen pääkaupunki?", "Kyllä");
    let out = h.submit(&a, c.clone()).await;
    let SubmitOutcome::Refused(report) = out else {
        panic!("{out:?}")
    };
    assert_eq!(report.errors, vec![RuleError::YesNo]);
    h.submit(&a, c).await; // retry: no second shaming
    h.deliver().await;
    let group = h.group_texts();
    assert_eq!(group.len(), 1);
    assert!(group[0].contains("Aino"));
    assert!(h.dm_texts(1).is_empty(), "never in a DM");
    assert_eq!(h.count("SELECT count(*) FROM question").await, 0);
}

#[tokio::test]
async fn numbers_boo_and_only_in_tiebreakers() {
    let h = h().await;
    let a = h.member(1, "Aino").await;
    let out = h
        .submit(&a, text("Kuinka monta luuta aikuisella on?", "206"))
        .await;
    assert!(
        matches!(out, SubmitOutcome::Refused(ref r) if r.errors == vec![RuleError::NumericOutsideTiebreaker])
    );
    let mut tb = text("Kuinka monta luuta aikuisella on?", "206");
    tb.kind = Kind::Tiebreaker;
    let qid = h.submitted(&a, tb).await;
    let year = h
        .submitted(&a, text("Minä vuonna Suomi itsenäistyi?", "1917"))
        .await;
    h.deliver().await;
    let boos: Vec<String> = h
        .group_texts()
        .into_iter()
        .filter(|t| t.starts_with("Boo!"))
        .collect();
    assert_eq!(
        boos.len(),
        3,
        "refused number, tiebreaker and year: {boos:?}"
    );
    assert_eq!(h.status(qid).await, "voting");
    assert_eq!(h.status(year).await, "voting");
}

#[tokio::test]
async fn duplicates_warn_then_confirm() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let first = h
        .submitted(&a, text("What is the capital of Finland?", "Helsinki"))
        .await;
    let c = text("What's the capital city of Finland?", "Helsinki");
    let out = questions::submit(&h.app, &a, c.clone(), false, h.now)
        .await
        .unwrap();
    let SubmitOutcome::Duplicates(m) = out else {
        panic!("{out:?}")
    };
    assert_eq!(m[0].question_id, first);
    let (_, report, dups) = questions::check_draft(&h.app, c.clone(), None)
        .await
        .unwrap();
    assert!(report.ok());
    assert_eq!(dups.len(), 1);
    let second = h.submitted(&a, c).await;
    h.deliver().await;
    let cards: Vec<String> = h.group_texts();
    assert!(
        cards[1].contains(&format!("Looks like a duplicate #{first}")),
        "{}",
        cards[1]
    );
    let d = questions::detail(&h.app, second).await.unwrap();
    assert_eq!(d.current.row.duplicates()[0].question_id, first);
}

#[tokio::test]
async fn unknown_theme_and_missing_media_are_refused() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let mut c = text("Q?", "A");
    c.theme = "astrology".into();
    let err = questions::submit(&h.app, &a, c, true, h.now)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Unknown theme"));
    let err = questions::submit(
        &h.app,
        &a,
        image_q("Who?", "Sibelius", "music", 99),
        true,
        h.now,
    )
    .await
    .unwrap_err();
    assert!(err.to_string().contains("Media #99"));
    // themes can be added
    let name = questions::add_theme(&h.app, "  Astrology  ", Some(a.id), h.now)
        .await
        .unwrap();
    assert_eq!(name, "astrology");
    assert!(
        questions::themes(&h.app)
            .await
            .unwrap()
            .contains(&"astrology".to_string())
    );
    assert!(
        questions::add_theme(&h.app, " ", None, h.now)
            .await
            .is_err()
    );
}

#[tokio::test]
async fn image_question_posts_media_before_card() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let m = media::store(&h.app, PNG, a.id, h.now).await.unwrap();
    let again = media::store(&h.app, PNG, a.id, h.now).await.unwrap();
    assert_eq!(m.id, again.id, "content-addressed");
    assert!(media::path_for(&h.app, &m.sha256).exists());
    h.submitted(&a, image_q("Who is this?", "Sibelius", "music", m.id))
        .await;
    h.deliver().await;
    let sent = h.tg.sent.lock().unwrap().clone();
    assert!(sent[0].1.file.is_some());
    assert!(sent[1].1.html.contains("1 media file(s) above"));
    assert!(media::store(&h.app, b"<html>", a.id, h.now).await.is_err());
}

#[tokio::test]
async fn author_edit_during_voting_reopens_on_material_change_only() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let b = h.member(2, "B").await;
    let c = h.member(3, "C").await;
    let qid = h.submitted(&a, text("Pisin joki?", "Niili")).await;
    questions::vote(&h.app, qid, None, &b, Vote::No, h.now)
        .await
        .unwrap();
    h.deliver().await;

    let mut minor = text("Pisin joki?", "Niili");
    minor.fun_fact = "Virtaa yhdentoista maan halki.".into();
    let out = questions::edit(&h.app, qid, &a, minor.clone(), "", h.now)
        .await
        .unwrap();
    assert!(matches!(
        out,
        EditOutcome::Applied {
            reopened: false,
            ..
        }
    ));
    let d = questions::detail(&h.app, qid).await.unwrap();
    assert_eq!(d.q.vote_round, 1);
    assert_eq!(d.rounds[0].votes.len(), 1, "votes kept");

    let mut major = minor.clone();
    major.prompt = "Mikä on maailman pisin joki?".into();
    let out = questions::edit(&h.app, qid, &a, major, "clearer", h.now)
        .await
        .unwrap();
    assert!(matches!(out, EditOutcome::Applied { reopened: true, .. }));
    let d = questions::detail(&h.app, qid).await.unwrap();
    assert_eq!(d.q.vote_round, 2);
    assert_eq!(d.rounds[0].votes.len(), 0, "new round, no votes");
    assert_eq!(d.rounds[1].row.outcome.as_deref(), Some("superseded"));
    assert_eq!(d.versions.len(), 3);
    assert_eq!(d.current.row.reason, "clearer");

    // the old card can't take votes any more
    let err = questions::vote(&h.app, qid, Some(1), &c, Vote::Yes, h.now)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("outdated"));
    questions::vote(&h.app, qid, Some(2), &c, Vote::Yes, h.now)
        .await
        .unwrap();
    h.deliver().await;
    let edits = h.tg.edited.lock().unwrap().clone();
    assert!(
        edits
            .iter()
            .any(|(_, _, m)| m.html.contains("Changed since") && m.html.contains("Pisin joki?"))
    );
    assert_eq!(
        questions::edit(
            &h.app,
            qid,
            &a,
            text("Mikä on maailman pisin joki?", "Niili"),
            "",
            h.now
        )
        .await
        .unwrap(),
        EditOutcome::Applied {
            version_id: 4,
            reopened: false
        }
    );
}

#[tokio::test]
async fn proposals_by_others_go_to_the_author() {
    let h = h().await;
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    let c = h.member(3, "Cy").await;
    let qid = h.accepted(&a, &b, text("Pisin joki?", "Niili")).await;
    let mut better = text("Pisin joki?", "Niili");
    better.fun_fact = "6650 km".into();
    let out = questions::edit(&h.app, qid, &b, better.clone(), "fun fact", h.now)
        .await
        .unwrap();
    let EditOutcome::Proposed(pid) = out else {
        panic!()
    };
    h.deliver().await;
    let dm =
        h.tg.sent
            .lock()
            .unwrap()
            .iter()
            .find(|(c, m)| *c == 1 && !m.buttons.is_empty())
            .cloned()
            .unwrap();
    assert!(dm.1.html.contains("Bo proposes a change"));
    assert_eq!(
        dm.1.buttons[0][0],
        Button::Callback {
            text: "✅ Accept".into(),
            data: format!("p:{pid}:a")
        }
    );

    // inbox
    assert_eq!(questions::inbox(&h.app, &a).await.unwrap().len(), 1);
    assert!(questions::inbox(&h.app, &c).await.unwrap().is_empty());
    // only the author decides
    let err = questions::decide(&h.app, pid, &c, true, h.now)
        .await
        .unwrap_err();
    assert!(matches!(err, AppError::Forbidden(_)));
    let err = questions::decide(&h.app, pid, &b, true, h.now)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("your own"));
    let out = questions::decide(&h.app, pid, &a, true, h.now)
        .await
        .unwrap();
    assert!(matches!(
        out,
        DecideOutcome::Accepted {
            version_id: Some(_)
        }
    ));
    let d = questions::detail(&h.app, qid).await.unwrap();
    assert_eq!(d.current.content.fun_fact, "6650 km");
    assert_eq!(d.q.status, "accepted", "non-material: stays accepted");
    assert!(d.current.row.reason.contains("accepted by Aino"));
    assert_eq!(d.proposals[0].changes[0].0, Field::FunFact);
    let err = questions::decide(&h.app, pid, &a, true, h.now)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("already accepted"));
    h.deliver().await;
    assert!(h.dm_texts(2).iter().any(|t| t.contains("was accepted")));

    // decline path
    let mut other = better.clone();
    other.tags = vec!["africa".into()];
    let EditOutcome::Proposed(p2) = questions::edit(&h.app, qid, &c, other, "", h.now)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(
        questions::decide(&h.app, p2, &a, false, h.now)
            .await
            .unwrap(),
        DecideOutcome::Declined
    );
    // withdraw path
    let mut third = better.clone();
    third.host_notes = "Niili, not Nile".into();
    let EditOutcome::Proposed(p3) = questions::edit(&h.app, qid, &c, third, "", h.now)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert!(questions::withdraw(&h.app, p3, &a, h.now).await.is_err());
    questions::withdraw(&h.app, p3, &c, h.now).await.unwrap();
    assert!(questions::withdraw(&h.app, p3, &c, h.now).await.is_err());
    // no change
    assert_eq!(
        questions::edit(&h.app, qid, &c, better, "", h.now)
            .await
            .unwrap(),
        EditOutcome::NoChange
    );
}

#[tokio::test]
async fn material_edit_by_author_of_accepted_needs_second_approval() {
    let h = h().await;
    let a = h.member(1, "Aino").await;
    let b = h.member(2, "Bo").await;
    let qid = h.accepted(&a, &b, text("Pisin joki?", "Niili")).await;
    let EditOutcome::Proposed(pid) = questions::edit(
        &h.app,
        qid,
        &a,
        text("Pisin joki?", "Amazon"),
        "new research",
        h.now,
    )
    .await
    .unwrap() else {
        panic!()
    };
    h.deliver().await;
    assert!(
        h.group_texts()
            .iter()
            .any(|t| t.contains("Another member needs to approve"))
    );
    assert!(
        questions::decide(&h.app, pid, &a, true, h.now)
            .await
            .is_err()
    );
    assert_eq!(questions::inbox(&h.app, &b).await.unwrap().len(), 1);
    questions::decide(&h.app, pid, &b, true, h.now)
        .await
        .unwrap();
    let d = questions::detail(&h.app, qid).await.unwrap();
    assert_eq!(d.current.content.answer, "Amazon");
    assert_eq!(d.q.status, "accepted");
    // non-material edit by the author applies directly
    let mut c = text("Pisin joki?", "Amazon");
    c.tags = vec!["rivers".into()];
    assert!(matches!(
        questions::edit(&h.app, qid, &a, c, "", h.now)
            .await
            .unwrap(),
        EditOutcome::Applied {
            reopened: false,
            ..
        }
    ));
}

#[tokio::test]
async fn stale_and_already_applied_proposals() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let b = h.member(2, "B").await;
    let c = h.member(3, "C").await;
    let qid = h.accepted(&a, &b, text("Pisin joki?", "Niili")).await;
    let mut pb = text("Pisin joki?", "Niili");
    pb.fun_fact = "B's fact".into();
    let mut pc = text("Pisin joki?", "Niili");
    pc.fun_fact = "C's fact".into();
    let EditOutcome::Proposed(p1) = questions::edit(&h.app, qid, &b, pb.clone(), "", h.now)
        .await
        .unwrap()
    else {
        panic!()
    };
    let EditOutcome::Proposed(p2) = questions::edit(&h.app, qid, &c, pc, "", h.now)
        .await
        .unwrap()
    else {
        panic!()
    };
    let EditOutcome::Proposed(p3) = questions::edit(&h.app, qid, &c, pb, "", h.now)
        .await
        .unwrap()
    else {
        panic!()
    };
    questions::decide(&h.app, p1, &a, true, h.now)
        .await
        .unwrap();
    assert_eq!(
        questions::decide(&h.app, p2, &a, true, h.now)
            .await
            .unwrap(),
        DecideOutcome::Stale(vec![Field::FunFact])
    );
    assert_eq!(
        questions::decide(&h.app, p3, &a, true, h.now)
            .await
            .unwrap(),
        DecideOutcome::Accepted { version_id: None }
    );
    h.deliver().await;
    assert!(
        h.dm_texts(3)
            .iter()
            .any(|t| t.contains("couldn't be applied"))
    );
}

#[tokio::test]
async fn proposal_breaking_rules_is_refused_at_edit_time() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let b = h.member(2, "B").await;
    let qid = h.accepted(&a, &b, text("Pisin joki?", "Niili")).await;
    let err = questions::edit(
        &h.app,
        qid,
        &b,
        text("Onko Niili pisin joki?", "Kyllä"),
        "",
        h.now,
    )
    .await
    .unwrap_err();
    let AppError::Rules(r) = err else {
        panic!("{err:?}")
    };
    assert_eq!(r.errors, vec![RuleError::YesNo]);
    h.deliver().await;
    assert_eq!(h.group_texts().len(), 2, "card + shame");
}

#[tokio::test]
async fn orphaned_question_can_be_decided_by_others() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let b = h.member(2, "B").await;
    let c = h.member(3, "C").await;
    let qid = h.accepted(&a, &b, text("Pisin joki?", "Niili")).await;
    h.tg.non_members.lock().unwrap().push(1);
    let gone = members::ensure_in_group(&h.app, &a, h.now).await.unwrap();
    assert!(!gone);
    let mut x = text("Pisin joki?", "Niili");
    x.tags = vec!["x".into()];
    let EditOutcome::Proposed(pid) = questions::edit(&h.app, qid, &b, x, "", h.now)
        .await
        .unwrap()
    else {
        panic!()
    };
    assert_eq!(questions::inbox(&h.app, &c).await.unwrap().len(), 1);
    questions::decide(&h.app, pid, &c, true, h.now)
        .await
        .unwrap();
}

#[tokio::test]
async fn rejected_can_be_revised_or_resubmitted() {
    let mut h = h().await;
    let a = h.member(1, "A").await;
    let b = h.member(2, "B").await;
    let qid = h.submitted(&a, text("Pisin joki?", "Niili")).await;
    let q2 = h.submitted(&a, text("Korkein vuori?", "Everest")).await;
    h.tick(Duration::days(8));
    questions::expire_votes(&h.app, h.now).await.unwrap();
    assert!(
        questions::resubmit(&h.app, qid, &b, h.now).await.is_err(),
        "author only"
    );
    assert_eq!(
        questions::resubmit(&h.app, qid, &a, h.now).await.unwrap(),
        2
    );
    assert_eq!(h.status(qid).await, "voting");
    assert!(
        questions::resubmit(&h.app, qid, &a, h.now).await.is_err(),
        "not rejected any more"
    );
    // revising a rejected question resubmits it
    let mut c = text("Korkein vuori?", "Everest");
    c.fun_fact = "8849 m".into();
    assert!(matches!(
        questions::edit(&h.app, q2, &a, c, "", h.now).await.unwrap(),
        EditOutcome::Applied { reopened: true, .. }
    ));
    assert_eq!(h.status(q2).await, "voting");
}

#[tokio::test]
async fn retire_and_bring_back() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let b = h.member(2, "B").await;
    let qid = h.accepted(&a, &b, text("Pisin joki?", "Niili")).await;
    assert!(
        questions::set_retired(&h.app, qid, &b, false, h.now)
            .await
            .is_err()
    );
    questions::set_retired(&h.app, qid, &b, true, h.now)
        .await
        .unwrap();
    assert_eq!(h.status(qid).await, "retired");
    questions::set_retired(&h.app, qid, &a, false, h.now)
        .await
        .unwrap();
    assert_eq!(h.status(qid).await, "accepted");
}

#[tokio::test]
async fn lists_detail_stats_random_notes() {
    let h = h().await;
    let a = h.member(1, "A").await;
    let b = h.member(2, "B").await;
    let q1 = h.accepted(&a, &b, text("Pisin joki?", "Niili")).await;
    let q2 = h
        .submitted(
            &a,
            themed(
                "Kuka sävelsi Finlandian?",
                "Sibelius",
                "music",
                Kind::Text,
                2,
            ),
        )
        .await;
    let q3 = h.submitted(&b, text("Korkein vuori?", "Everest")).await;

    let all = questions::list(&h.app, &Filter::default()).await.unwrap();
    assert_eq!(all.len(), 3);
    let music = questions::list(
        &h.app,
        &Filter {
            theme: Some("music".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(music.iter().map(|s| s.id).collect::<Vec<_>>(), vec![q2]);
    let found = questions::list(
        &h.app,
        &Filter {
            text: Some("sibel".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(found.len(), 1);
    let todo = questions::list(
        &h.app,
        &Filter {
            needs_vote_by: Some(b.id),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(todo.iter().map(|s| s.id).collect::<Vec<_>>(), vec![q2]);
    let mine = questions::list(
        &h.app,
        &Filter {
            author_id: Some(b.id),
            status: Some("voting".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(mine[0].id, q3);
    assert!(mine[0].closes_at.is_some());
    let filtered = questions::list(
        &h.app,
        &Filter {
            kind: Some("text".into()),
            difficulty: Some(2),
            language: Some("fi".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(filtered.len(), 1);
    assert!(
        questions::list(
            &h.app,
            &Filter {
                flagged: true,
                ..Default::default()
            }
        )
        .await
        .unwrap()
        .is_empty()
    );

    let n = questions::add_note(&h.app, q1, &b, "Check spelling", Some(q3), h.now)
        .await
        .unwrap();
    assert!(n > 0);
    assert!(
        questions::add_note(&h.app, q1, &b, " ", None, h.now)
            .await
            .is_err()
    );
    let d = questions::detail(&h.app, q3).await.unwrap();
    assert_eq!(d.notes.len(), 1, "linked notes show on both");

    let s = questions::stats(&h.app).await.unwrap();
    assert_eq!(
        s.by_status,
        vec![("accepted".into(), 1), ("voting".into(), 2)]
    );
    assert_eq!(s.by_theme, vec![("geography".into(), 1)]);
    assert_eq!(s.top_authors[0], ("A".into(), 2));

    let c = h.member(3, "C").await;
    let (id, content) = questions::random_accepted(&h.app, &c, None, 7, h.now)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(id, q1);
    assert_eq!(content.answer, "Niili");
    assert!(
        questions::random_accepted(&h.app, &c, Some("music"), 0, h.now)
            .await
            .unwrap()
            .is_none()
    );
    assert!(matches!(
        questions::detail(&h.app, 999).await,
        Err(AppError::NotFound)
    ));
}
