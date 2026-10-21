//! Deterministic near-duplicate detection (DESIGN.md §6). Warns, never blocks.

use super::text::{content_words, normalize, trigram_similarity};
use serde::{Deserialize, Serialize};

pub const LIKELY_SCORE: f64 = 0.80;
pub const LIKELY_PROMPT: f64 = 0.90;
pub const SAME_ANSWER_PROMPT: f64 = 0.35;
pub const SIMILAR_SCORE: f64 = 0.55;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Band {
    LikelyDuplicate,
    SameAnswer,
    Similar,
}

impl Band {
    pub fn label(self) -> &'static str {
        match self {
            Band::LikelyDuplicate => "Looks like a duplicate",
            Band::SameAnswer => "Same answer, might overlap",
            Band::Similar => "Similar",
        }
    }
}

/// What dedupe needs to know about an existing question.
#[derive(Debug, Clone)]
pub struct Candidate {
    pub question_id: i64,
    pub status: String,
    pub language: String,
    pub prompt: String,
    pub answers: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Match {
    pub question_id: i64,
    pub status: String,
    pub band: Band,
    pub score: f64,
    pub q_sim: f64,
    pub a_sim: f64,
}

/// Pre-normalized text for one side of a comparison.
#[derive(Debug, Clone)]
pub struct Prepared {
    pub prompt: String,
    pub answers: Vec<String>,
}

pub fn prepare(prompt: &str, answers: &[String], language: &str) -> Prepared {
    Prepared {
        prompt: content_words(prompt, language),
        answers: answers
            .iter()
            .map(|a| normalize(a))
            .filter(|a| !a.is_empty())
            .collect(),
    }
}

/// (q_sim, a_sim, score) between two prepared questions.
pub fn compare(a: &Prepared, b: &Prepared) -> (f64, f64, f64) {
    let q_sim = trigram_similarity(&a.prompt, &b.prompt);
    let exact = a.answers.iter().any(|x| b.answers.contains(x));
    let a_sim = if exact {
        1.0
    } else {
        match (a.answers.first(), b.answers.first()) {
            (Some(x), Some(y)) => trigram_similarity(x, y),
            _ => 0.0,
        }
    };
    (q_sim, a_sim, 0.7 * q_sim + 0.3 * a_sim)
}

pub fn band(q_sim: f64, a_sim: f64, score: f64) -> Option<Band> {
    if score >= LIKELY_SCORE || q_sim >= LIKELY_PROMPT {
        Some(Band::LikelyDuplicate)
    } else if a_sim >= 1.0 && q_sim >= SAME_ANSWER_PROMPT {
        Some(Band::SameAnswer)
    } else if score >= SIMILAR_SCORE {
        Some(Band::Similar)
    } else {
        None
    }
}

/// True when two questions would be flagged as likely duplicates; quiz fill
/// uses this to keep two versions of the same fact out of one quiz.
pub fn likely_duplicates(a: &Prepared, b: &Prepared) -> bool {
    let (q, a_, s) = compare(a, b);
    band(q, a_, s) == Some(Band::LikelyDuplicate)
}

/// Every candidate in a band, best first; ties by question id. `exclude` is the
/// question being edited, which shouldn't match itself.
pub fn find_similar(
    prompt: &str,
    answers: &[String],
    language: &str,
    candidates: &[Candidate],
    exclude: Option<i64>,
    limit: usize,
) -> Vec<Match> {
    let me = prepare(prompt, answers, language);
    let mut out: Vec<Match> = candidates
        .iter()
        .filter(|c| Some(c.question_id) != exclude)
        .filter_map(|c| {
            let other = prepare(&c.prompt, &c.answers, &c.language);
            let (q_sim, a_sim, score) = compare(&me, &other);
            band(q_sim, a_sim, score).map(|band| Match {
                question_id: c.question_id,
                status: c.status.clone(),
                band,
                score: round3(score),
                q_sim: round3(q_sim),
                a_sim: round3(a_sim),
            })
        })
        .collect();
    out.sort_by(|x, y| {
        y.score
            .total_cmp(&x.score)
            .then(x.question_id.cmp(&y.question_id))
    });
    out.truncate(limit);
    out
}

fn round3(x: f64) -> f64 {
    (x * 1000.0).round() / 1000.0
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cand(id: i64, prompt: &str, answer: &str) -> Candidate {
        Candidate {
            question_id: id,
            status: "accepted".into(),
            language: "en".into(),
            prompt: prompt.into(),
            answers: vec![answer.into()],
        }
    }

    fn bank() -> Vec<Candidate> {
        vec![
            cand(1, "What is the capital of Finland?", "Helsinki"),
            cand(2, "Which city hosted the 1952 Summer Olympics?", "Helsinki"),
            cand(3, "Who composed Finlandia?", "Jean Sibelius"),
            cand(4, "What is the capital of Sweden?", "Stockholm"),
        ]
    }

    #[test]
    fn exact_rewording_is_likely_duplicate() {
        let m = find_similar(
            "What's the capital city of Finland?",
            &["Helsinki".into()],
            "en",
            &bank(),
            None,
            10,
        );
        assert_eq!(m[0].question_id, 1);
        assert_eq!(m[0].band, Band::LikelyDuplicate);
        assert!(m.iter().all(|x| x.question_id != 3));
    }

    #[test]
    fn same_answer_different_question_is_flagged() {
        let m = find_similar(
            "Which Finnish city hosted the Olympic Games?",
            &["Helsinki".into()],
            "en",
            &bank(),
            None,
            10,
        );
        assert!(
            m.iter()
                .any(|x| x.question_id == 2 && x.band == Band::SameAnswer),
            "{m:?}"
        );
        // same answer but an unrelated prompt is not worth a warning
        let far = find_similar(
            "Which city is home to the Olympic Stadium designed by Lindegren?",
            &["Helsinki".into()],
            "en",
            &bank(),
            None,
            10,
        );
        assert!(far.iter().all(|x| x.question_id != 2), "{far:?}");
    }

    #[test]
    fn alternative_answers_count_as_exact() {
        let a = prepare(
            "Capital of Finland",
            &["Helsinki".into(), "Helsingfors".into()],
            "en",
        );
        let b = prepare("Capital of Finland", &["helsingfors".into()], "en");
        let (_, a_sim, _) = compare(&a, &b);
        assert_eq!(a_sim, 1.0);
        assert!(likely_duplicates(&a, &b));
    }

    #[test]
    fn unrelated_question_matches_nothing() {
        let m = find_similar(
            "Which planet has the most moons?",
            &["Saturn".into()],
            "en",
            &bank(),
            None,
            10,
        );
        assert!(m.is_empty(), "{m:?}");
    }

    #[test]
    fn exclude_and_limit_and_stable_order() {
        let mut b = bank();
        b.push(cand(9, "What is the capital of Finland?", "Helsinki"));
        let m = find_similar(
            "What is the capital of Finland?",
            &["Helsinki".into()],
            "en",
            &b,
            Some(1),
            1,
        );
        assert_eq!(m.len(), 1);
        assert_eq!(m[0].question_id, 9);
        let all = find_similar(
            "What is the capital of Finland?",
            &["Helsinki".into()],
            "en",
            &b,
            None,
            10,
        );
        assert_eq!(all[0].question_id, 1, "equal scores order by id");
        assert_eq!(all[1].question_id, 9);
    }

    #[test]
    fn bands_and_labels() {
        assert_eq!(band(0.95, 0.0, 0.665), Some(Band::LikelyDuplicate));
        assert_eq!(band(0.5, 1.0, 0.65), Some(Band::SameAnswer));
        assert_eq!(band(0.7, 0.4, 0.61), Some(Band::Similar));
        assert_eq!(band(0.2, 0.2, 0.2), None);
        for b in [Band::LikelyDuplicate, Band::SameAnswer, Band::Similar] {
            assert!(!b.label().is_empty());
        }
        let p = prepare("x", &[], "en");
        assert_eq!(compare(&p, &p).1, 0.0);
    }
}
