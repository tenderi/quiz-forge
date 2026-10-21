//! Question content and the field-level patches that change it.

use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Text,
    MultipleChoice,
    MultiPart,
    Image,
    Audio,
    Tiebreaker,
}

impl Kind {
    pub const ALL: [Kind; 6] = [
        Kind::Text,
        Kind::MultipleChoice,
        Kind::MultiPart,
        Kind::Image,
        Kind::Audio,
        Kind::Tiebreaker,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Kind::Text => "text",
            Kind::MultipleChoice => "multiple_choice",
            Kind::MultiPart => "multi_part",
            Kind::Image => "image",
            Kind::Audio => "audio",
            Kind::Tiebreaker => "tiebreaker",
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            Kind::Text => "Text",
            Kind::MultipleChoice => "Multiple choice",
            Kind::MultiPart => "Multi-part",
            Kind::Image => "Image",
            Kind::Audio => "Audio",
            Kind::Tiebreaker => "Tiebreaker",
        }
    }
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Kind {
    type Err = String;
    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Kind::ALL
            .into_iter()
            .find(|k| k.as_str() == s)
            .ok_or_else(|| format!("unknown question kind '{s}'"))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Part {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub answer: String,
    pub points: u32,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MediaKind {
    Image,
    Audio,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MediaRef {
    pub media_id: i64,
    pub kind: MediaKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub start_s: Option<f64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_s: Option<f64>,
}

/// Everything a question version holds. Optional free-text fields use an empty
/// string for "not set", so a patch can clear them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Content {
    pub kind: Kind,
    pub prompt: String,
    pub answer: String,
    #[serde(default)]
    pub accepted_answers: Vec<String>,
    #[serde(default)]
    pub choices: Vec<String>,
    #[serde(default)]
    pub parts: Vec<Part>,
    pub points: u32,
    #[serde(default)]
    pub media: Vec<MediaRef>,
    pub difficulty: u8,
    pub theme: String,
    #[serde(default)]
    pub tags: Vec<String>,
    pub language: String,
    #[serde(default)]
    pub background: String,
    #[serde(default)]
    pub fun_fact: String,
    #[serde(default)]
    pub host_notes: String,
    #[serde(default)]
    pub sources: Vec<String>,
}

impl Content {
    /// A minimal text question; handy as a starting point for wizards and tests.
    pub fn text(prompt: &str, answer: &str, theme: &str, language: &str) -> Content {
        Content {
            kind: Kind::Text,
            prompt: prompt.to_string(),
            answer: answer.to_string(),
            accepted_answers: vec![],
            choices: vec![],
            parts: vec![],
            points: 1,
            media: vec![],
            difficulty: 3,
            theme: theme.to_string(),
            tags: vec![],
            language: language.to_string(),
            background: String::new(),
            fun_fact: String::new(),
            host_notes: String::new(),
            sources: vec![],
        }
    }

    /// Trims text fields, drops empty list entries, lowercases and dedupes tags.
    /// Applied before validation and storage so equal content compares equal.
    pub fn normalized(mut self) -> Content {
        fn clean_list(v: Vec<String>) -> Vec<String> {
            v.into_iter()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty())
                .collect()
        }
        self.prompt = self.prompt.trim().to_string();
        self.answer = self.answer.trim().to_string();
        self.accepted_answers = clean_list(self.accepted_answers);
        self.choices = clean_list(self.choices);
        for p in &mut self.parts {
            p.answer = p.answer.trim().to_string();
            p.prompt = p
                .prompt
                .take()
                .map(|s| s.trim().to_string())
                .filter(|s| !s.is_empty());
        }
        self.parts.retain(|p| !p.answer.is_empty());
        if self.kind == Kind::MultiPart && !self.parts.is_empty() {
            self.points = self.parts.iter().map(|p| p.points).sum();
            if self.answer.is_empty() {
                let answers: Vec<&str> = self.parts.iter().map(|p| p.answer.as_str()).collect();
                self.answer = answers.join(", ");
            }
        }
        self.theme = self.theme.trim().to_string();
        let mut tags: Vec<String> = Vec::new();
        for t in self.tags {
            let t = t.trim().trim_start_matches('#').to_lowercase();
            if !t.is_empty() && !tags.contains(&t) {
                tags.push(t);
            }
        }
        self.tags = tags;
        self.language = self.language.trim().to_lowercase();
        self.background = self.background.trim().to_string();
        self.fun_fact = self.fun_fact.trim().to_string();
        self.host_notes = self.host_notes.trim().to_string();
        self.sources = clean_list(self.sources);
        self
    }

    /// All answers a host should accept, canonical first.
    pub fn all_answers(&self) -> Vec<&str> {
        let mut v = vec![self.answer.as_str()];
        v.extend(self.accepted_answers.iter().map(String::as_str));
        v
    }
}

/// A field of [`Content`], for diffs and permission rules.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Field {
    Kind,
    Prompt,
    Answer,
    AcceptedAnswers,
    Choices,
    Parts,
    Points,
    Media,
    Difficulty,
    Theme,
    Tags,
    Language,
    Background,
    FunFact,
    HostNotes,
    Sources,
}

impl Field {
    pub const ALL: [Field; 16] = [
        Field::Kind,
        Field::Prompt,
        Field::Answer,
        Field::AcceptedAnswers,
        Field::Choices,
        Field::Parts,
        Field::Points,
        Field::Media,
        Field::Difficulty,
        Field::Theme,
        Field::Tags,
        Field::Language,
        Field::Background,
        Field::FunFact,
        Field::HostNotes,
        Field::Sources,
    ];

    /// Material fields change what is asked or what counts as right (§5.3).
    pub fn is_material(self) -> bool {
        matches!(
            self,
            Field::Kind
                | Field::Prompt
                | Field::Answer
                | Field::AcceptedAnswers
                | Field::Choices
                | Field::Parts
                | Field::Media
        )
    }

    pub fn label(self) -> &'static str {
        match self {
            Field::Kind => "Kind",
            Field::Prompt => "Question",
            Field::Answer => "Answer",
            Field::AcceptedAnswers => "Accepted answers",
            Field::Choices => "Choices",
            Field::Parts => "Parts",
            Field::Points => "Points",
            Field::Media => "Media",
            Field::Difficulty => "Difficulty",
            Field::Theme => "Theme",
            Field::Tags => "Tags",
            Field::Language => "Language",
            Field::Background => "Background",
            Field::FunFact => "Fun fact",
            Field::HostNotes => "Host notes",
            Field::Sources => "Sources",
        }
    }

    fn get(self, c: &Content) -> serde_json::Value {
        use serde_json::to_value as v;
        match self {
            Field::Kind => v(c.kind),
            Field::Prompt => v(&c.prompt),
            Field::Answer => v(&c.answer),
            Field::AcceptedAnswers => v(&c.accepted_answers),
            Field::Choices => v(&c.choices),
            Field::Parts => v(&c.parts),
            Field::Points => v(c.points),
            Field::Media => v(&c.media),
            Field::Difficulty => v(c.difficulty),
            Field::Theme => v(&c.theme),
            Field::Tags => v(&c.tags),
            Field::Language => v(&c.language),
            Field::Background => v(&c.background),
            Field::FunFact => v(&c.fun_fact),
            Field::HostNotes => v(&c.host_notes),
            Field::Sources => v(&c.sources),
        }
        .expect("content fields serialize")
    }

    fn set(self, c: &mut Content, val: serde_json::Value) -> Result<(), serde_json::Error> {
        use serde_json::from_value as f;
        match self {
            Field::Kind => c.kind = f(val)?,
            Field::Prompt => c.prompt = f(val)?,
            Field::Answer => c.answer = f(val)?,
            Field::AcceptedAnswers => c.accepted_answers = f(val)?,
            Field::Choices => c.choices = f(val)?,
            Field::Parts => c.parts = f(val)?,
            Field::Points => c.points = f(val)?,
            Field::Media => c.media = f(val)?,
            Field::Difficulty => c.difficulty = f(val)?,
            Field::Theme => c.theme = f(val)?,
            Field::Tags => c.tags = f(val)?,
            Field::Language => c.language = f(val)?,
            Field::Background => c.background = f(val)?,
            Field::FunFact => c.fun_fact = f(val)?,
            Field::HostNotes => c.host_notes = f(val)?,
            Field::Sources => c.sources = f(val)?,
        }
        Ok(())
    }
}

/// A set of field replacements. Ordered, so its JSON form is stable.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Patch(pub std::collections::BTreeMap<Field, serde_json::Value>);

impl Patch {
    /// The patch that turns `from` into `to`.
    pub fn between(from: &Content, to: &Content) -> Patch {
        let mut m = std::collections::BTreeMap::new();
        for f in Field::ALL {
            let (a, b) = (f.get(from), f.get(to));
            if a != b {
                m.insert(f, b);
            }
        }
        Patch(m)
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn fields(&self) -> impl Iterator<Item = Field> + '_ {
        self.0.keys().copied()
    }

    pub fn is_material(&self) -> bool {
        self.fields().any(Field::is_material)
    }

    pub fn apply(&self, base: &Content) -> Result<Content, serde_json::Error> {
        let mut c = base.clone();
        for (f, v) in &self.0 {
            f.set(&mut c, v.clone())?;
        }
        Ok(c)
    }

    /// The old and new value of every patched field, for display.
    pub fn describe(&self, base: &Content) -> Vec<(Field, serde_json::Value, serde_json::Value)> {
        self.0
            .iter()
            .map(|(f, v)| (*f, f.get(base), v.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Content {
        Content::text("Capital of Finland?", "Helsinki", "geography", "en")
    }

    #[test]
    fn kind_round_trips_through_strings() {
        for k in Kind::ALL {
            assert_eq!(k.as_str().parse::<Kind>().unwrap(), k);
            assert_eq!(k.to_string(), k.as_str());
            assert!(!k.label().is_empty());
        }
        assert!("quiz".parse::<Kind>().is_err());
    }

    #[test]
    fn normalized_trims_and_cleans() {
        let mut c = sample();
        c.prompt = "  Capital?  ".into();
        c.tags = vec![
            "#Europe".into(),
            "europe".into(),
            " ".into(),
            "Capitals".into(),
        ];
        c.accepted_answers = vec!["".into(), " Hki ".into()];
        c.language = " FI ".into();
        let n = c.normalized();
        assert_eq!(n.prompt, "Capital?");
        assert_eq!(n.tags, vec!["europe", "capitals"]);
        assert_eq!(n.accepted_answers, vec!["Hki"]);
        assert_eq!(n.language, "fi");
    }

    #[test]
    fn normalized_multi_part_sums_points_and_drops_blank_parts() {
        let mut c = sample();
        c.kind = Kind::MultiPart;
        c.parts = vec![
            Part {
                prompt: Some("  ".into()),
                answer: " A ".into(),
                points: 1,
            },
            Part {
                prompt: None,
                answer: "B".into(),
                points: 2,
            },
            Part {
                prompt: None,
                answer: " ".into(),
                points: 5,
            },
        ];
        let n = c.normalized();
        assert_eq!(n.parts.len(), 2);
        assert_eq!(n.parts[0].prompt, None);
        assert_eq!(n.parts[0].answer, "A");
        assert_eq!(n.points, 3);
    }

    #[test]
    fn patch_between_and_apply_round_trip() {
        let a = sample();
        let mut b = a.clone();
        b.answer = "Helsingfors".into();
        b.difficulty = 1;
        b.fun_fact = "Founded 1550.".into();
        let p = Patch::between(&a, &b);
        assert_eq!(
            p.fields().collect::<Vec<_>>(),
            vec![Field::Answer, Field::Difficulty, Field::FunFact]
        );
        assert!(p.is_material());
        assert_eq!(p.apply(&a).unwrap(), b);
        assert!(Patch::between(&a, &a).is_empty());
        let d = p.describe(&a);
        assert_eq!(d[0].1, serde_json::json!("Helsinki"));
        assert_eq!(d[0].2, serde_json::json!("Helsingfors"));
    }

    #[test]
    fn non_material_patch() {
        let a = sample();
        let mut b = a.clone();
        b.tags = vec!["nordic".into()];
        b.host_notes = "say it slowly".into();
        assert!(!Patch::between(&a, &b).is_material());
    }

    #[test]
    fn every_field_has_label_and_roundtrips() {
        let a = sample();
        for f in Field::ALL {
            assert!(!f.label().is_empty());
            let mut c = a.clone();
            f.set(&mut c, f.get(&a)).unwrap();
            assert_eq!(c, a);
        }
    }

    #[test]
    fn patch_json_is_stable_and_applies_bad_value_errors() {
        let mut m = std::collections::BTreeMap::new();
        m.insert(Field::Difficulty, serde_json::json!("hard"));
        let p = Patch(m);
        assert!(p.apply(&sample()).is_err());
        let json = serde_json::to_string(&p).unwrap();
        assert_eq!(json, r#"{"difficulty":"hard"}"#);
    }

    #[test]
    fn all_answers_lists_canonical_first() {
        let mut c = sample();
        c.accepted_answers = vec!["Helsingfors".into()];
        assert_eq!(c.all_answers(), vec!["Helsinki", "Helsingfors"]);
    }
}
