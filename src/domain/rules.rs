//! House rules (DESIGN.md §4.3): hard validation of a question version, plus
//! the reactions the group gets to see. Pure and deterministic.

use super::model::{Content, Kind, MediaKind};
use super::text::{LANGUAGE, normalize, tokens};
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::sync::LazyLock;

/// What kind of answer a question has, as far as the house rules care.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerClass {
    Plain,
    YesNo,
    Numeric,
    Year,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "code")]
pub enum RuleError {
    EmptyPrompt,
    EmptyAnswer,
    BadDifficulty,
    MissingTheme,
    BadLanguage,
    ZeroPoints,
    YesNo,
    NumericOutsideTiebreaker,
    TiebreakerNotNumeric,
    ChoiceCount { count: usize },
    DuplicateChoices,
    AnswerNotAChoice,
    ChoicesOnlyForMultipleChoice,
    MediaOnMultipleChoice,
    TooFewParts,
    PartsOnlyForMultiPart,
    ImageRequired,
    AudioRequired,
    WrongMediaKind,
    MediaNotAllowed,
}

impl RuleError {
    pub fn message(&self) -> String {
        match self {
            RuleError::EmptyPrompt => "The question text is empty.".into(),
            RuleError::EmptyAnswer => "The answer is empty.".into(),
            RuleError::BadDifficulty => "Difficulty must be 1–5.".into(),
            RuleError::MissingTheme => "Pick a theme.".into(),
            RuleError::BadLanguage => "Language must be a code like 'fi' or 'en'.".into(),
            RuleError::ZeroPoints => "A question is worth at least one point.".into(),
            RuleError::YesNo => "Yes/no questions aren't allowed. Ask for the thing itself.".into(),
            RuleError::NumericOutsideTiebreaker => {
                "Number answers are only allowed in tiebreakers (years excepted). Use the tiebreaker kind, or ask for something else.".into()
            }
            RuleError::TiebreakerNotNumeric => {
                "A tiebreaker needs a number (or year) answer, so the closest guess wins.".into()
            }
            RuleError::ChoiceCount { count } => {
                format!("Multiple choice needs 3–6 choices; this has {count}.")
            }
            RuleError::DuplicateChoices => "Two choices are the same.".into(),
            RuleError::AnswerNotAChoice => "The answer must be one of the choices.".into(),
            RuleError::ChoicesOnlyForMultipleChoice => {
                "Only multiple-choice questions have choices.".into()
            }
            RuleError::MediaOnMultipleChoice => {
                "Multiple choice is text only; make picture questions open-answer.".into()
            }
            RuleError::TooFewParts => "A multi-part question needs at least two parts.".into(),
            RuleError::PartsOnlyForMultiPart => "Only multi-part questions have parts.".into(),
            RuleError::ImageRequired => "An image question needs at least one image.".into(),
            RuleError::AudioRequired => "An audio question needs exactly one audio clip.".into(),
            RuleError::WrongMediaKind => "The attached media doesn't match the question kind.".into(),
            RuleError::MediaNotAllowed => {
                "Only image and audio questions carry media.".into()
            }
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Reaction {
    /// Yes/no attempt: a shaming message in the group.
    Shame,
    /// Any numeric answer, years included: "Boo!" in the group.
    Boo,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RuleReport {
    pub answer_class: AnswerClass,
    pub errors: Vec<RuleError>,
    pub reactions: Vec<Reaction>,
}

impl RuleReport {
    pub fn ok(&self) -> bool {
        self.errors.is_empty()
    }
}

struct Compiled {
    yes_no: BTreeSet<String>,
    number_words: BTreeSet<String>,
    approximators: BTreeSet<String>,
    units: BTreeSet<String>,
    era: BTreeSet<String>,
    decade_suffixes: BTreeSet<String>,
    year_prompt: Vec<Regex>,
    quantity_prompt: Vec<Regex>,
    digits_unit: Regex,
}

static C: LazyLock<Compiled> = LazyLock::new(|| {
    let set = |v: &Vec<String>| v.iter().map(|s| normalize(s)).collect::<BTreeSet<_>>();
    let re = |v: &Vec<String>| {
        v.iter()
            .map(|p| Regex::new(p).expect("valid regex in data/language.toml"))
            .collect::<Vec<_>>()
    };
    let l = &*LANGUAGE;
    Compiled {
        yes_no: set(&l.yes_no.words),
        number_words: set(&l.numbers.words),
        approximators: set(&l.numbers.approximators),
        units: set(&l.numbers.units),
        era: set(&l.years.era_markers),
        decade_suffixes: set(&l.years.decade_suffixes),
        year_prompt: re(&l.years.prompt_patterns),
        quantity_prompt: re(&l.numbers.quantity_patterns),
        digits_unit: Regex::new(r"^(\d+)(\p{L}+)$").unwrap(),
    }
});

fn is_digits(t: &str) -> bool {
    !t.is_empty() && t.chars().all(|c| c.is_ascii_digit())
}

/// Classifies an answer given its question. See DESIGN.md §4.3 for the rules;
/// they're heuristics on purpose, and the AI pass catches what they miss.
pub fn classify_answer(prompt: &str, answer: &str) -> AnswerClass {
    let c = &*C;
    let norm = normalize(answer);
    if c.yes_no.contains(&norm) {
        return AnswerClass::YesNo;
    }
    let mut toks = tokens(&norm);
    while toks.first().is_some_and(|t| c.approximators.contains(*t)) {
        toks.remove(0);
    }
    if toks.is_empty() {
        return AnswerClass::Plain;
    }
    let lower_prompt = prompt.to_lowercase();
    let asks_year = c.year_prompt.iter().any(|r| r.is_match(&lower_prompt));
    let asks_quantity = c.quantity_prompt.iter().any(|r| r.is_match(&lower_prompt));

    let has_digits = toks.iter().any(|t| t.chars().any(|ch| ch.is_ascii_digit()));
    if has_digits && toks.len() <= 4 && toks.iter().any(|t| c.era.contains(*t)) {
        let rest_ok = toks.iter().all(|t| is_digits(t) || c.era.contains(*t));
        if rest_ok {
            return AnswerClass::Year;
        }
    }
    // Decades: "1960s", "1960-luvulla", "1960-talet".
    if toks.len() <= 2 {
        let t0 = toks[0];
        let decade_suffix = t0.len() >= 4
            && t0.ends_with('s')
            && is_digits(&t0[..t0.len() - 1])
            && (3..=4).contains(&(t0.len() - 1));
        let decade_word = toks.len() == 2
            && is_digits(t0)
            && (3..=4).contains(&t0.len())
            && c.decade_suffixes.contains(toks[1]);
        if decade_suffix || decade_word {
            return AnswerClass::Year;
        }
    }

    // Digits first: "42", "1 000 000", "3.14", "42 km", "42km".
    if toks[0].chars().next().is_some_and(|ch| ch.is_ascii_digit()) {
        let mut num: Vec<&str> = Vec::new();
        let mut rest: Vec<String> = Vec::new();
        for t in &toks {
            if rest.is_empty() && is_digits(t) {
                num.push(t);
            } else if rest.is_empty()
                && let Some(cap) = c.digits_unit.captures(t)
            {
                num.push(cap.get(1).unwrap().as_str());
                rest.push(cap[2].to_string());
            } else {
                rest.push((*t).to_string());
            }
        }
        let units_ok = rest.len() <= 2 && rest.iter().all(|t| c.units.contains(t));
        if !num.is_empty() && units_ok {
            let single_int = num.len() == 1 && rest.is_empty();
            if single_int
                && asks_year
                && let Ok(y) = num[0].parse::<u32>()
                && (1..=2200).contains(&y)
            {
                return AnswerClass::Year;
            }
            return AnswerClass::Numeric;
        }
        return AnswerClass::Plain;
    }

    // Number words only count when the question asks for a quantity.
    if asks_quantity {
        let words: Vec<&&str> = toks.iter().filter(|t| !c.units.contains(**t)).collect();
        if !words.is_empty()
            && words.len() <= 4
            && words.iter().all(|t| c.number_words.contains(**t))
        {
            return AnswerClass::Numeric;
        }
    }
    AnswerClass::Plain
}

fn language_ok(lang: &str) -> bool {
    !lang.is_empty()
        && lang.len() <= 16
        && lang.chars().all(|c| c.is_ascii_alphanumeric() || c == '-')
        && lang.chars().next().is_some_and(|c| c.is_ascii_alphabetic())
}

/// Runs every house rule. Expects [`Content::normalized`] input.
pub fn check(c: &Content) -> RuleReport {
    let mut errors = Vec::new();
    if c.prompt.is_empty() {
        errors.push(RuleError::EmptyPrompt);
    }
    let has_parts = c.kind == Kind::MultiPart && !c.parts.is_empty();
    if c.answer.is_empty() && !has_parts {
        errors.push(RuleError::EmptyAnswer);
    }
    if !(1..=5).contains(&c.difficulty) {
        errors.push(RuleError::BadDifficulty);
    }
    if c.theme.is_empty() {
        errors.push(RuleError::MissingTheme);
    }
    if !language_ok(&c.language) {
        errors.push(RuleError::BadLanguage);
    }
    if c.points == 0 {
        errors.push(RuleError::ZeroPoints);
    }

    // Kind-specific shape.
    if c.kind == Kind::MultipleChoice {
        if !(3..=6).contains(&c.choices.len()) {
            errors.push(RuleError::ChoiceCount {
                count: c.choices.len(),
            });
        }
        let normed: Vec<String> = c.choices.iter().map(|s| normalize(s)).collect();
        let uniq: BTreeSet<&String> = normed.iter().collect();
        if uniq.len() != normed.len() {
            errors.push(RuleError::DuplicateChoices);
        }
        if !c.answer.is_empty() && !normed.contains(&normalize(&c.answer)) {
            errors.push(RuleError::AnswerNotAChoice);
        }
        if !c.media.is_empty() {
            errors.push(RuleError::MediaOnMultipleChoice);
        }
    } else if !c.choices.is_empty() {
        errors.push(RuleError::ChoicesOnlyForMultipleChoice);
    }
    if c.kind == Kind::MultiPart {
        if c.parts.len() < 2 {
            errors.push(RuleError::TooFewParts);
        }
    } else if !c.parts.is_empty() {
        errors.push(RuleError::PartsOnlyForMultiPart);
    }
    match c.kind {
        Kind::Image => {
            if c.media.is_empty() {
                errors.push(RuleError::ImageRequired);
            } else if c.media.iter().any(|m| m.kind != MediaKind::Image) {
                errors.push(RuleError::WrongMediaKind);
            }
        }
        Kind::Audio => {
            if c.media.len() != 1 {
                errors.push(RuleError::AudioRequired);
            } else if c.media[0].kind != MediaKind::Audio {
                errors.push(RuleError::WrongMediaKind);
            }
        }
        Kind::MultipleChoice => {} // reported above
        _ => {
            if !c.media.is_empty() {
                errors.push(RuleError::MediaNotAllowed);
            }
        }
    }

    // The two house rules proper.
    let mut class = classify_answer(&c.prompt, &c.answer);
    if c.kind == Kind::MultipleChoice {
        let yn = c
            .choices
            .iter()
            .filter(|s| C.yes_no.contains(&normalize(s)))
            .count();
        if yn >= 2 {
            class = AnswerClass::YesNo;
        }
    }
    if c.kind == Kind::MultiPart {
        // A list of answers is never a yes/no or a lone number.
        class = AnswerClass::Plain;
    }
    let mut reactions = Vec::new();
    match class {
        AnswerClass::YesNo => {
            errors.push(RuleError::YesNo);
            reactions.push(Reaction::Shame);
        }
        AnswerClass::Numeric => {
            if c.kind != Kind::Tiebreaker {
                errors.push(RuleError::NumericOutsideTiebreaker);
            }
            reactions.push(Reaction::Boo);
        }
        AnswerClass::Year => reactions.push(Reaction::Boo),
        AnswerClass::Plain => {
            if c.kind == Kind::Tiebreaker {
                errors.push(RuleError::TiebreakerNotNumeric);
            }
        }
    }
    RuleReport {
        answer_class: class,
        errors,
        reactions,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::model::{MediaRef, Part};

    fn q(prompt: &str, answer: &str) -> Content {
        Content::text(prompt, answer, "misc", "en").normalized()
    }

    #[test]
    fn classify_plain_answers() {
        for (p, a) in [
            ("Capital of Finland?", "Helsinki"),
            ("Which rapper is named after a coin?", "50 Cent"),
            ("Which soda has a number in its name?", "7 Up"),
            ("Which Fincher film has a sin theme?", "Seven"),
            ("Which band sang 'All the Small Things'?", "Blink-182"),
            ("Name the element", "Neon 10"),
            ("Say nothing", "…"),
        ] {
            assert_eq!(classify_answer(p, a), AnswerClass::Plain, "{a}");
        }
    }

    #[test]
    fn classify_yes_no_in_many_languages() {
        for a in [
            "Yes", "no.", "Kyllä!", "ei", "TOTTA", "Nej", "true", "Correct",
        ] {
            assert_eq!(classify_answer("Is it?", a), AnswerClass::YesNo, "{a}");
        }
    }

    #[test]
    fn classify_numbers() {
        for (p, a) in [
            ("How many bones in an adult human?", "206"),
            ("Length of the Danube?", "2 850 km"),
            ("Pi to two decimals?", "3.14"),
            ("Height of Halti?", "1324m"),
            ("How tall is it?", "about 40 metres"),
            ("Kuinka monta?", "noin 7"),
            ("Population?", "1,000,000"),
            ("How many wonders of the ancient world?", "seven"),
            ("Kuinka monta jalkaa hämähäkillä on?", "kahdeksan"),
            ("Montako?", "kolme sataa"),
            ("Steps in the tower?", "1665"),
            ("How old was she?", "100 years"),
        ] {
            assert_eq!(classify_answer(p, a), AnswerClass::Numeric, "{a}");
        }
    }

    #[test]
    fn classify_years() {
        for (p, a) in [
            ("In which year did Finland become independent?", "1917"),
            ("Minä vuonna Suomi itsenäistyi?", "1917"),
            ("When was the Colosseum completed?", "80"),
            ("When did Caesar die?", "44 BC"),
            ("Rome was founded?", "753 eKr."),
            ("Era of disco?", "1970s"),
            ("Milloin tämä oli muotia?", "1960-luvulla"),
            ("Vilket decennium?", "1960-talet"),
        ] {
            assert_eq!(classify_answer(p, a), AnswerClass::Year, "{a}");
        }
        // a year-shaped number without a year question is just a number
        assert_eq!(
            classify_answer("Steps in the tower?", "1665"),
            AnswerClass::Numeric
        );
        // a big number in a year question is still a number
        assert_eq!(
            classify_answer("In which year?", "12000"),
            AnswerClass::Numeric
        );
    }

    #[test]
    fn good_text_question_passes() {
        let r = check(&q("Capital of Finland?", "Helsinki"));
        assert!(r.ok(), "{r:?}");
        assert!(r.reactions.is_empty());
    }

    #[test]
    fn yes_no_is_refused_with_shame() {
        let r = check(&q("Is Helsinki the capital of Finland?", "Yes"));
        assert_eq!(r.errors, vec![RuleError::YesNo]);
        assert_eq!(r.reactions, vec![Reaction::Shame]);
    }

    #[test]
    fn yes_no_multiple_choice_is_refused() {
        let mut c = q("Is it true?", "True");
        c.kind = Kind::MultipleChoice;
        c.choices = vec!["True".into(), "False".into(), "Maybe".into()];
        let r = check(&c);
        assert!(r.errors.contains(&RuleError::YesNo));
        assert_eq!(r.answer_class, AnswerClass::YesNo);
    }

    #[test]
    fn numeric_only_in_tiebreaker_and_always_boo() {
        let mut c = q("How many bones in an adult human?", "206");
        let r = check(&c);
        assert_eq!(r.errors, vec![RuleError::NumericOutsideTiebreaker]);
        assert_eq!(r.reactions, vec![Reaction::Boo]);
        c.kind = Kind::Tiebreaker;
        let r = check(&c);
        assert!(r.ok());
        assert_eq!(r.reactions, vec![Reaction::Boo]);
    }

    #[test]
    fn year_allowed_anywhere_but_boo() {
        let r = check(&q("In which year did Finland become independent?", "1917"));
        assert!(r.ok());
        assert_eq!(r.reactions, vec![Reaction::Boo]);
        let mut c = q("In which year did Finland become independent?", "1917");
        c.kind = Kind::Tiebreaker;
        assert!(check(&c).ok());
    }

    #[test]
    fn tiebreaker_needs_number() {
        let mut c = q("Capital?", "Helsinki");
        c.kind = Kind::Tiebreaker;
        assert_eq!(check(&c).errors, vec![RuleError::TiebreakerNotNumeric]);
    }

    #[test]
    fn multiple_choice_shape() {
        let mut c = q("Largest lake in Finland?", "Saimaa");
        c.kind = Kind::MultipleChoice;
        c.choices = vec!["Saimaa".into(), "Päijänne".into(), "Inari".into()];
        assert!(check(&c).ok());

        c.choices = vec!["Saimaa".into(), "Inari".into()];
        assert!(
            check(&c)
                .errors
                .contains(&RuleError::ChoiceCount { count: 2 })
        );

        c.choices = vec!["Saimaa".into(), "saimaa!".into(), "Inari".into()];
        assert!(check(&c).errors.contains(&RuleError::DuplicateChoices));

        c.choices = vec!["Oulujärvi".into(), "Päijänne".into(), "Inari".into()];
        assert!(check(&c).errors.contains(&RuleError::AnswerNotAChoice));

        c.choices = vec!["Saimaa".into(), "Päijänne".into(), "Inari".into()];
        c.media = vec![MediaRef {
            media_id: 1,
            kind: MediaKind::Image,
            start_s: None,
            end_s: None,
        }];
        assert!(check(&c).errors.contains(&RuleError::MediaOnMultipleChoice));
    }

    #[test]
    fn choices_and_parts_only_on_their_kinds() {
        let mut c = q("Capital?", "Helsinki");
        c.choices = vec!["a".into()];
        c.parts = vec![Part {
            prompt: None,
            answer: "x".into(),
            points: 1,
        }];
        let e = check(&c).errors;
        assert!(e.contains(&RuleError::ChoicesOnlyForMultipleChoice));
        assert!(e.contains(&RuleError::PartsOnlyForMultiPart));
    }

    #[test]
    fn multi_part_needs_two_parts_and_derives_answer() {
        let mut c = Content::text("Name the Nordic capitals", "", "geography", "en");
        c.kind = Kind::MultiPart;
        c.parts = vec![
            Part {
                prompt: None,
                answer: "Helsinki".into(),
                points: 1,
            },
            Part {
                prompt: None,
                answer: "Oslo".into(),
                points: 1,
            },
        ];
        let c = c.normalized();
        assert_eq!(c.answer, "Helsinki, Oslo");
        assert_eq!(c.points, 2);
        assert!(check(&c).ok());
        let mut one = c.clone();
        one.parts.truncate(1);
        assert!(check(&one).errors.contains(&RuleError::TooFewParts));
    }

    #[test]
    fn media_rules() {
        let img = MediaRef {
            media_id: 1,
            kind: MediaKind::Image,
            start_s: None,
            end_s: None,
        };
        let aud = MediaRef {
            media_id: 2,
            kind: MediaKind::Audio,
            start_s: Some(3.0),
            end_s: None,
        };

        let mut c = q("Who is this?", "Sibelius");
        c.kind = Kind::Image;
        assert_eq!(check(&c).errors, vec![RuleError::ImageRequired]);
        c.media = vec![img.clone()];
        assert!(check(&c).ok());
        c.media = vec![aud.clone()];
        assert_eq!(check(&c).errors, vec![RuleError::WrongMediaKind]);

        c.kind = Kind::Audio;
        assert!(check(&c).ok());
        c.media = vec![img.clone()];
        assert_eq!(check(&c).errors, vec![RuleError::WrongMediaKind]);
        c.media = vec![];
        assert_eq!(check(&c).errors, vec![RuleError::AudioRequired]);

        c.kind = Kind::Text;
        c.media = vec![img];
        assert_eq!(check(&c).errors, vec![RuleError::MediaNotAllowed]);
    }

    #[test]
    fn basic_field_errors() {
        let mut c = Content::text("", "", "", "").normalized();
        c.difficulty = 9;
        c.points = 0;
        let e = check(&c).errors;
        for want in [
            RuleError::EmptyPrompt,
            RuleError::EmptyAnswer,
            RuleError::BadDifficulty,
            RuleError::MissingTheme,
            RuleError::BadLanguage,
            RuleError::ZeroPoints,
        ] {
            assert!(e.contains(&want), "missing {want:?}");
        }
        assert!(!language_ok("1x"));
        assert!(language_ok("pt-br"));
    }

    #[test]
    fn every_error_has_a_message() {
        let all = [
            RuleError::EmptyPrompt,
            RuleError::EmptyAnswer,
            RuleError::BadDifficulty,
            RuleError::MissingTheme,
            RuleError::BadLanguage,
            RuleError::ZeroPoints,
            RuleError::YesNo,
            RuleError::NumericOutsideTiebreaker,
            RuleError::TiebreakerNotNumeric,
            RuleError::ChoiceCount { count: 1 },
            RuleError::DuplicateChoices,
            RuleError::AnswerNotAChoice,
            RuleError::ChoicesOnlyForMultipleChoice,
            RuleError::MediaOnMultipleChoice,
            RuleError::TooFewParts,
            RuleError::PartsOnlyForMultiPart,
            RuleError::ImageRequired,
            RuleError::AudioRequired,
            RuleError::WrongMediaKind,
            RuleError::MediaNotAllowed,
        ];
        for e in all {
            assert!(e.message().ends_with('.'), "{e:?}");
        }
        let json = serde_json::to_string(&RuleError::ChoiceCount { count: 2 }).unwrap();
        assert_eq!(json, r#"{"code":"choice_count","count":2}"#);
    }
}
