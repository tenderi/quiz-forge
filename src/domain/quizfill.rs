//! Quiz templates and the seeded fill (DESIGN.md §9). Same template, same seed,
//! same bank snapshot and same locks give the same quiz, on any machine.

use super::dedupe::{Prepared, likely_duplicates};
use super::model::Kind;
use super::text::stable_hash;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Template {
    pub name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    #[serde(default = "yes")]
    pub exclude_used: bool,
    #[serde(default)]
    pub spoilers: SpoilerMode,
    #[serde(rename = "round")]
    pub rounds: Vec<RoundSpec>,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpoilerMode {
    #[default]
    Exclude,
    Warn,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoundSpec {
    pub title: String,
    #[serde(rename = "slot")]
    pub slots: Vec<SlotSpec>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum Difficulty {
    Exactly(u8),
    Range([u8; 2]),
}

impl Difficulty {
    pub fn bounds(&self) -> (u8, u8) {
        match self {
            Difficulty::Exactly(d) => (*d, *d),
            Difficulty::Range([a, b]) => (*a, *b),
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SlotSpec {
    #[serde(default = "one")]
    pub count: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub kind: Option<Kind>,
    /// A theme name, or `$name` for "one theme, chosen by the fill, shared by
    /// every slot of this round that uses `$name`".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub theme: Option<String>,
    /// Any one of these themes.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub themes: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub difficulty: Option<Difficulty>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags_any: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub tags_none: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
}

fn one() -> u32 {
    1
}

impl SlotSpec {
    fn variable(&self) -> Option<&str> {
        self.theme.as_deref().and_then(|t| t.strip_prefix('$'))
    }
}

pub const MAX_SLOTS: u32 = 500;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TemplateError {
    #[error("template is not valid TOML: {0}")]
    Parse(String),
    #[error("the template needs a name")]
    NoName,
    #[error("the template needs at least one round")]
    NoRounds,
    #[error("round {0} has no slots")]
    EmptyRound(usize),
    #[error("round {round}: slot count must be 1–{MAX_SLOTS}")]
    BadCount { round: usize },
    #[error("round {round}: difficulty must be within 1–5, low to high")]
    BadDifficulty { round: usize },
    #[error("round {round}: '{name}' is not a valid variable name")]
    BadVariable { round: usize, name: String },
    #[error("round {round}: use either `theme` or `themes`, not both")]
    ThemeAndThemes { round: usize },
    #[error("the template has more than {MAX_SLOTS} questions")]
    TooBig,
}

impl Template {
    pub fn parse(src: &str) -> Result<Template, TemplateError> {
        let t: Template =
            toml::from_str(src).map_err(|e| TemplateError::Parse(e.message().to_string()))?;
        t.validate()?;
        Ok(t)
    }

    pub fn to_toml(&self) -> String {
        toml::to_string_pretty(self).expect("templates serialize")
    }

    pub fn validate(&self) -> Result<(), TemplateError> {
        if self.name.trim().is_empty() {
            return Err(TemplateError::NoName);
        }
        if self.rounds.is_empty() {
            return Err(TemplateError::NoRounds);
        }
        let mut total = 0u32;
        for (i, r) in self.rounds.iter().enumerate() {
            let round = i + 1;
            if r.slots.is_empty() {
                return Err(TemplateError::EmptyRound(round));
            }
            for s in &r.slots {
                if s.count == 0 || s.count > MAX_SLOTS {
                    return Err(TemplateError::BadCount { round });
                }
                total += s.count;
                if let Some(d) = &s.difficulty {
                    let (lo, hi) = d.bounds();
                    if lo < 1 || hi > 5 || lo > hi {
                        return Err(TemplateError::BadDifficulty { round });
                    }
                }
                if let Some(v) = s.variable()
                    && (v.is_empty() || !v.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'))
                {
                    return Err(TemplateError::BadVariable {
                        round,
                        name: v.to_string(),
                    });
                }
                if s.theme.is_some() && !s.themes.is_empty() {
                    return Err(TemplateError::ThemeAndThemes { round });
                }
            }
        }
        if total > MAX_SLOTS {
            return Err(TemplateError::TooBig);
        }
        Ok(())
    }

    /// Slots per round after expanding `count`.
    pub fn shape(&self) -> Vec<usize> {
        self.rounds
            .iter()
            .map(|r| r.slots.iter().map(|s| s.count as usize).sum())
            .collect()
    }

    /// The spec of every expanded slot, per round.
    pub fn expanded(&self) -> Vec<Vec<&SlotSpec>> {
        self.rounds
            .iter()
            .map(|r| {
                r.slots
                    .iter()
                    .flat_map(|s| std::iter::repeat_n(s, s.count as usize))
                    .collect()
            })
            .collect()
    }

    pub fn example() -> &'static str {
        include_str!("../../data/template_example.toml")
    }
}

/// An accepted question as the fill sees it.
#[derive(Debug, Clone)]
pub struct Cand {
    pub question_id: i64,
    pub kind: Kind,
    pub theme: String,
    pub difficulty: u8,
    pub tags: Vec<String>,
    pub language: String,
    /// Used at an event that was held.
    pub used: bool,
    /// Seen by one of the expected players (§8).
    pub spoiled: bool,
    pub dedupe: Prepared,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FilledSlot {
    pub question_id: Option<i64>,
    pub locked: bool,
    pub empty_reason: Option<String>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Filled {
    pub rounds: Vec<Vec<FilledSlot>>,
    /// `"<round>.$<var>"` → theme chosen.
    pub bindings: BTreeMap<String, String>,
}

/// One theme tried for a variable: the theme, each open slot's pick, how many filled.
type Attempt = (String, Vec<(usize, Option<i64>)>, usize);

/// Position of a slot: (round index, slot index), both 0-based.
pub type SlotPos = (usize, usize);

fn slot_path(pos: SlotPos) -> String {
    format!("r{}.s{}", pos.0 + 1, pos.1 + 1)
}

fn rank(seed: &str, path: &str, id: &str) -> String {
    stable_hash(&[seed, path, id])
}

struct Filler<'a> {
    t: &'a Template,
    cands: &'a [Cand],
    by_id: BTreeMap<i64, &'a Cand>,
    seed: &'a str,
    picked: BTreeSet<i64>,
}

impl<'a> Filler<'a> {
    fn matches(&self, c: &Cand, s: &SlotSpec, theme: Option<&str>) -> bool {
        if let Some(k) = s.kind {
            if c.kind != k {
                return false;
            }
        } else if c.kind == Kind::Tiebreaker {
            // Tiebreakers only go where the template asks for one.
            return false;
        }
        match (theme, &s.theme) {
            (Some(t), _) => {
                if c.theme != t {
                    return false;
                }
            }
            (None, Some(t)) if !t.starts_with('$') && &c.theme != t => {
                return false;
            }
            _ => {}
        }
        if !s.themes.is_empty() && !s.themes.contains(&c.theme) {
            return false;
        }
        if let Some(d) = &s.difficulty {
            let (lo, hi) = d.bounds();
            if c.difficulty < lo || c.difficulty > hi {
                return false;
            }
        }
        if !s.tags_any.is_empty() && !s.tags_any.iter().any(|t| c.tags.contains(t)) {
            return false;
        }
        if s.tags_none.iter().any(|t| c.tags.contains(t)) {
            return false;
        }
        if let Some(l) = s.language.as_ref().or(self.t.language.as_ref())
            && &c.language != l
        {
            return false;
        }
        true
    }

    fn available(&self, c: &Cand, picked: &BTreeSet<i64>) -> bool {
        if picked.contains(&c.question_id) {
            return false;
        }
        if self.t.exclude_used && c.used {
            return false;
        }
        if self.t.spoilers == SpoilerMode::Exclude && c.spoiled {
            return false;
        }
        !picked
            .iter()
            .filter_map(|id| self.by_id.get(id))
            .any(|p| likely_duplicates(&p.dedupe, &c.dedupe))
    }

    /// Candidates for a slot, best first.
    fn ranked(
        &self,
        pos: SlotPos,
        s: &SlotSpec,
        theme: Option<&str>,
        picked: &BTreeSet<i64>,
    ) -> Vec<&'a Cand> {
        let path = slot_path(pos);
        let mut v: Vec<(String, &Cand)> = self
            .cands
            .iter()
            .filter(|c| self.matches(c, s, theme) && self.available(c, picked))
            .map(|c| (rank(self.seed, &path, &c.question_id.to_string()), c))
            .collect();
        v.sort_by(|a, b| a.0.cmp(&b.0));
        v.into_iter().map(|(_, c)| c).collect()
    }

    fn empty_reason(&self, s: &SlotSpec, theme: Option<&str>) -> String {
        let n = self
            .cands
            .iter()
            .filter(|c| self.matches(c, s, theme))
            .count();
        if n == 0 {
            let mut parts = Vec::new();
            if let Some(k) = s.kind {
                parts.push(format!("kind {}", k.label().to_lowercase()));
            }
            if let Some(t) = theme.or(s.theme.as_deref()) {
                parts.push(format!("theme “{t}”"));
            }
            if !s.themes.is_empty() {
                parts.push(format!("themes {}", s.themes.join("/")));
            }
            if let Some(d) = &s.difficulty {
                let (lo, hi) = d.bounds();
                parts.push(if lo == hi {
                    format!("difficulty {lo}")
                } else {
                    format!("difficulty {lo}–{hi}")
                });
            }
            if let Some(l) = s.language.as_ref().or(self.t.language.as_ref()) {
                parts.push(format!("language {l}"));
            }
            if parts.is_empty() {
                "No accepted questions in the bank".to_string()
            } else {
                format!("No accepted questions with {}", parts.join(", "))
            }
        } else {
            format!(
                "All {n} matching questions are used, spoiled, near-duplicates or already in this quiz"
            )
        }
    }

    fn warnings(&self, c: &Cand) -> Vec<String> {
        let mut w = Vec::new();
        if c.spoiled {
            w.push("An expected player has seen this question".to_string());
        }
        if c.used {
            w.push("Used at an earlier event".to_string());
        }
        w
    }
}

/// Fills a template. `locks` are slots the host set by hand; they're kept as is.
pub fn fill(t: &Template, cands: &[Cand], seed: &str, locks: &BTreeMap<SlotPos, i64>) -> Filled {
    let mut f = Filler {
        t,
        cands,
        by_id: cands.iter().map(|c| (c.question_id, c)).collect(),
        seed,
        picked: locks.values().copied().collect(),
    };
    let expanded = t.expanded();
    let mut rounds: Vec<Vec<Option<FilledSlot>>> =
        expanded.iter().map(|r| vec![None; r.len()]).collect();
    let mut bindings = BTreeMap::new();

    for (pos, id) in locks {
        if let Some(slot) = rounds.get_mut(pos.0).and_then(|r| r.get_mut(pos.1)) {
            *slot = Some(FilledSlot {
                question_id: Some(*id),
                locked: true,
                empty_reason: None,
                warnings: vec![],
            });
        }
    }

    for (ri, specs) in expanded.iter().enumerate() {
        // Variables first, in order of first use.
        let mut vars: Vec<&str> = Vec::new();
        for s in specs {
            if let Some(v) = s.variable()
                && !vars.contains(&v)
            {
                vars.push(v);
            }
        }
        for var in vars {
            let slots: Vec<usize> = (0..specs.len())
                .filter(|&si| specs[si].variable() == Some(var))
                .collect();
            let forced = slots.iter().find_map(|&si| {
                locks
                    .get(&(ri, si))
                    .and_then(|id| f.by_id.get(id))
                    .map(|c| c.theme.clone())
            });
            let open: Vec<usize> = slots
                .iter()
                .copied()
                .filter(|si| !locks.contains_key(&(ri, *si)))
                .collect();
            let key = format!("r{}.${var}", ri + 1);

            let themes: Vec<String> = match forced {
                Some(t) => vec![t],
                None => {
                    let all: BTreeSet<&str> = cands.iter().map(|c| c.theme.as_str()).collect();
                    let mut v: Vec<(String, &str)> = all
                        .into_iter()
                        .map(|th| (rank(seed, &key, th), th))
                        .collect();
                    v.sort();
                    v.into_iter().map(|(_, th)| th.to_string()).collect()
                }
            };
            // Try each theme on a scratch copy; keep the first that fills every slot,
            // else the one that fills the most.
            let mut best: Option<Attempt> = None;
            for th in &themes {
                let mut picked = f.picked.clone();
                let mut got = Vec::new();
                for &si in &open {
                    let pick = f
                        .ranked((ri, si), specs[si], Some(th), &picked)
                        .first()
                        .map(|c| c.question_id);
                    if let Some(id) = pick {
                        picked.insert(id);
                    }
                    got.push((si, pick));
                }
                let n = got.iter().filter(|(_, p)| p.is_some()).count();
                let better = best.as_ref().is_none_or(|(_, _, bn)| n > *bn);
                if better {
                    best = Some((th.clone(), got, n));
                }
                if n == open.len() {
                    break;
                }
            }
            match best {
                Some((th, got, _)) => {
                    for (si, pick) in got {
                        rounds[ri][si] = Some(match pick {
                            Some(id) => {
                                f.picked.insert(id);
                                FilledSlot {
                                    question_id: Some(id),
                                    locked: false,
                                    empty_reason: None,
                                    warnings: f.warnings(f.by_id[&id]),
                                }
                            }
                            None => FilledSlot {
                                question_id: None,
                                locked: false,
                                empty_reason: Some(f.empty_reason(specs[si], Some(&th))),
                                warnings: vec![],
                            },
                        });
                    }
                    bindings.insert(key, th);
                }
                None => {
                    for si in open {
                        rounds[ri][si] = Some(FilledSlot {
                            question_id: None,
                            locked: false,
                            empty_reason: Some("No themes in the bank to choose from".to_string()),
                            warnings: vec![],
                        });
                    }
                }
            }
        }
        // Then everything else, in order.
        for (si, spec) in specs.iter().enumerate() {
            if rounds[ri][si].is_some() {
                continue;
            }
            let pick = f.ranked((ri, si), spec, None, &f.picked).first().copied();
            rounds[ri][si] = Some(match pick {
                Some(c) => {
                    f.picked.insert(c.question_id);
                    FilledSlot {
                        question_id: Some(c.question_id),
                        locked: false,
                        empty_reason: None,
                        warnings: f.warnings(c),
                    }
                }
                None => FilledSlot {
                    question_id: None,
                    locked: false,
                    empty_reason: Some(f.empty_reason(spec, None)),
                    warnings: vec![],
                },
            });
        }
    }
    Filled {
        rounds: rounds
            .into_iter()
            .map(|r| {
                r.into_iter()
                    .map(|s| s.expect("every slot filled"))
                    .collect()
            })
            .collect(),
        bindings,
    }
}

/// Questions that could go into one slot instead of its current one, best
/// first, for the "swap" list. `taken` is every question elsewhere in the quiz.
pub fn swap_candidates(
    t: &Template,
    cands: &[Cand],
    seed: &str,
    pos: SlotPos,
    bound_theme: Option<&str>,
    taken: &BTreeSet<i64>,
    limit: usize,
) -> Vec<i64> {
    let expanded = t.expanded();
    let Some(spec) = expanded.get(pos.0).and_then(|r| r.get(pos.1)) else {
        return vec![];
    };
    let f = Filler {
        t,
        cands,
        by_id: cands.iter().map(|c| (c.question_id, c)).collect(),
        seed,
        picked: BTreeSet::new(),
    };
    f.ranked(pos, spec, bound_theme, taken)
        .into_iter()
        .take(limit)
        .map(|c| c.question_id)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::dedupe::prepare;

    fn cand(id: i64, kind: Kind, theme: &str, difficulty: u8) -> Cand {
        let prompt = format!("Question number {id} about {theme} xq{id}zz");
        Cand {
            question_id: id,
            kind,
            theme: theme.into(),
            difficulty,
            tags: vec![],
            language: "fi".into(),
            used: false,
            spoiled: false,
            dedupe: prepare(&prompt, &[format!("answer{id}")], "fi"),
        }
    }

    fn bank() -> Vec<Cand> {
        let mut v = Vec::new();
        let themes = ["music", "history", "sport"];
        for i in 0..60 {
            let theme = themes[(i % 3) as usize];
            let kind = if i % 5 == 0 { Kind::Image } else { Kind::Text };
            v.push(cand(i + 1, kind, theme, (i % 5 + 1) as u8));
        }
        v.push(cand(100, Kind::Tiebreaker, "misc", 3));
        v
    }

    fn tpl(src: &str) -> Template {
        Template::parse(src).unwrap()
    }

    const NIGHT: &str = r#"
        name = "Night"
        [[round]]
        title = "Warm-up"
        [[round.slot]]
        count = 4
        difficulty = [1, 2]
        [[round]]
        title = "Pictures"
        [[round.slot]]
        count = 3
        kind = "image"
        theme = "$a"
        [[round.slot]]
        count = 2
        [[round]]
        title = "Tiebreaker"
        [[round.slot]]
        kind = "tiebreaker"
    "#;

    #[test]
    fn example_template_parses() {
        let t = Template::parse(Template::example()).unwrap();
        assert!(t.rounds.len() >= 3);
        assert_eq!(Template::parse(&t.to_toml()).unwrap(), t);
    }

    #[test]
    fn fill_is_deterministic_and_respects_constraints() {
        let t = tpl(NIGHT);
        let b = bank();
        let a = fill(&t, &b, "seed-1", &BTreeMap::new());
        let again = fill(&t, &b, "seed-1", &BTreeMap::new());
        assert_eq!(a, again);
        assert_eq!(t.shape(), vec![4, 5, 1]);
        let by_id: BTreeMap<i64, &Cand> = b.iter().map(|c| (c.question_id, c)).collect();
        for s in &a.rounds[0] {
            let c = by_id[&s.question_id.unwrap()];
            assert!(c.difficulty <= 2 && c.kind != Kind::Tiebreaker);
        }
        let pic_themes: BTreeSet<&str> = a.rounds[1][..3]
            .iter()
            .map(|s| by_id[&s.question_id.unwrap()].theme.as_str())
            .collect();
        assert_eq!(pic_themes.len(), 1, "image slots share a theme");
        for s in &a.rounds[1][..3] {
            assert_eq!(by_id[&s.question_id.unwrap()].kind, Kind::Image);
        }
        assert_eq!(
            a.bindings.get("r2.$a").map(String::as_str),
            pic_themes.iter().next().copied()
        );
        assert_eq!(a.rounds[2][0].question_id, Some(100));
        // all distinct
        let ids: Vec<i64> = a
            .rounds
            .iter()
            .flatten()
            .filter_map(|s| s.question_id)
            .collect();
        assert_eq!(ids.len(), ids.iter().collect::<BTreeSet<_>>().len());
        // another seed gives another quiz
        let b2 = fill(&t, &b, "seed-2", &BTreeMap::new());
        assert_ne!(a.rounds, b2.rounds);
    }

    #[test]
    fn insertion_order_of_bank_does_not_matter() {
        let t = tpl(NIGHT);
        let mut b = bank();
        let a = fill(&t, &b, "s", &BTreeMap::new());
        b.reverse();
        assert_eq!(fill(&t, &b, "s", &BTreeMap::new()), a);
    }

    #[test]
    fn locks_are_kept_and_force_variable_theme() {
        let t = tpl(NIGHT);
        let b = bank();
        let img = b
            .iter()
            .find(|c| c.kind == Kind::Image && c.theme == "history")
            .unwrap()
            .question_id;
        let mut locks = BTreeMap::new();
        locks.insert((1, 1), img);
        let a = fill(&t, &b, "seed-1", &locks);
        assert_eq!(a.rounds[1][1].question_id, Some(img));
        assert!(a.rounds[1][1].locked);
        assert_eq!(a.bindings["r2.$a"], "history");
        let refill = fill(&t, &b, "other", &locks);
        assert_eq!(refill.rounds[1][1].question_id, Some(img));
    }

    #[test]
    fn variable_falls_back_to_best_theme_when_none_fits() {
        let t = tpl(r#"
            name = "x"
            [[round]]
            title = "Pics"
            [[round.slot]]
            count = 9
            kind = "image"
            theme = "$a"
        "#);
        // each theme has 4 images; the fill takes 4 and explains the rest
        let a = fill(&t, &bank(), "s", &BTreeMap::new());
        let filled = a.rounds[0]
            .iter()
            .filter(|s| s.question_id.is_some())
            .count();
        assert_eq!(filled, 4);
        let reason = a.rounds[0]
            .iter()
            .find_map(|s| s.empty_reason.clone())
            .unwrap();
        assert!(reason.starts_with("All 4 matching"), "{reason}");
    }

    #[test]
    fn empty_bank_gives_reasons() {
        let t = tpl(NIGHT);
        let a = fill(&t, &[], "s", &BTreeMap::new());
        assert!(
            a.rounds
                .iter()
                .flatten()
                .all(|s| s.question_id.is_none() && s.empty_reason.is_some())
        );
        assert_eq!(
            a.rounds[1][0].empty_reason.as_deref(),
            Some("No themes in the bank to choose from")
        );
        assert_eq!(
            a.rounds[0][0].empty_reason.as_deref(),
            Some("No accepted questions with difficulty 1–2")
        );
        assert_eq!(
            a.rounds[2][0].empty_reason.as_deref(),
            Some("No accepted questions with kind tiebreaker")
        );
    }

    #[test]
    fn used_and_spoiled_excluded_or_warned() {
        let mut b = vec![
            cand(1, Kind::Text, "music", 3),
            cand(2, Kind::Text, "music", 3),
        ];
        b[0].used = true;
        b[1].spoiled = true;
        let src = r#"
            name = "x"
            [[round]]
            title = "R"
            [[round.slot]]
            count = 2
        "#;
        let a = fill(&tpl(src), &b, "s", &BTreeMap::new());
        assert!(a.rounds[0].iter().all(|s| s.question_id.is_none()));

        let mut t = tpl(src);
        t.exclude_used = false;
        t.spoilers = SpoilerMode::Warn;
        let a = fill(&t, &b, "s", &BTreeMap::new());
        assert!(
            a.rounds[0]
                .iter()
                .all(|s| s.question_id.is_some() && !s.warnings.is_empty())
        );
    }

    #[test]
    fn near_duplicates_never_share_a_quiz() {
        let mut b = vec![
            cand(1, Kind::Text, "geo", 3),
            cand(2, Kind::Text, "geo", 3),
            cand(3, Kind::Text, "geo", 3),
        ];
        b[0].dedupe = prepare(
            "What is the capital of Finland?",
            &["Helsinki".into()],
            "en",
        );
        b[1].dedupe = prepare(
            "What's the capital city of Finland?",
            &["Helsinki".into()],
            "en",
        );
        let src = r#"
            name = "x"
            [[round]]
            title = "R"
            [[round.slot]]
            count = 3
        "#;
        let a = fill(&tpl(src), &b, "s", &BTreeMap::new());
        let ids: BTreeSet<i64> = a.rounds[0].iter().filter_map(|s| s.question_id).collect();
        assert_eq!(ids.len(), 2);
        assert!(ids.contains(&3));
    }

    #[test]
    fn slot_filters() {
        let mut b = bank();
        b[1].tags = vec!["finland".into()];
        b[2].language = "en".into();
        let t = tpl(r#"
            name = "x"
            language = "fi"
            [[round]]
            title = "R"
            [[round.slot]]
            tags_any = ["finland"]
            [[round.slot]]
            themes = ["sport"]
            tags_none = ["finland"]
            [[round.slot]]
            theme = "history"
            difficulty = 4
            [[round.slot]]
            language = "en"
        "#);
        let a = fill(&t, &b, "s", &BTreeMap::new());
        let by_id: BTreeMap<i64, &Cand> = b.iter().map(|c| (c.question_id, c)).collect();
        let got: Vec<&Cand> = a.rounds[0]
            .iter()
            .map(|s| by_id[&s.question_id.unwrap()])
            .collect();
        assert_eq!(got[0].question_id, b[1].question_id);
        assert_eq!(got[1].theme, "sport");
        assert_eq!((got[2].theme.as_str(), got[2].difficulty), ("history", 4));
        assert_eq!(got[3].language, "en");
    }

    #[test]
    fn swap_candidates_skip_taken_and_follow_spec() {
        let t = tpl(NIGHT);
        let b = bank();
        let taken: BTreeSet<i64> = [1, 2].into();
        let s = swap_candidates(&t, &b, "s", (0, 0), None, &taken, 5);
        assert_eq!(s.len(), 5);
        assert!(s.iter().all(|id| !taken.contains(id)));
        let by_id: BTreeMap<i64, &Cand> = b.iter().map(|c| (c.question_id, c)).collect();
        assert!(s.iter().all(|id| by_id[id].difficulty <= 2));
        let pics = swap_candidates(&t, &b, "s", (1, 0), Some("music"), &taken, 50);
        assert!(
            pics.iter()
                .all(|id| by_id[id].kind == Kind::Image && by_id[id].theme == "music")
        );
        assert!(swap_candidates(&t, &b, "s", (9, 9), None, &taken, 5).is_empty());
    }

    #[test]
    fn template_validation_errors() {
        let base = "name = \"x\"\n[[round]]\ntitle = \"R\"\n[[round.slot]]\n";
        assert_eq!(
            Template::parse("name = \"\"\nround = []"),
            Err(TemplateError::NoName)
        );
        assert_eq!(
            Template::parse("name = \"x\"\nround = []"),
            Err(TemplateError::NoRounds)
        );
        assert_eq!(
            Template::parse("name = \"x\"\n[[round]]\ntitle = \"R\"\nslot = []"),
            Err(TemplateError::EmptyRound(1))
        );
        assert_eq!(
            Template::parse(&format!("{base}count = 0")),
            Err(TemplateError::BadCount { round: 1 })
        );
        assert_eq!(
            Template::parse(&format!("{base}difficulty = [4, 2]")),
            Err(TemplateError::BadDifficulty { round: 1 })
        );
        assert_eq!(
            Template::parse(&format!("{base}theme = \"$a b\"")),
            Err(TemplateError::BadVariable {
                round: 1,
                name: "a b".into()
            })
        );
        assert_eq!(
            Template::parse(&format!("{base}theme = \"x\"\nthemes = [\"y\"]")),
            Err(TemplateError::ThemeAndThemes { round: 1 })
        );
        assert_eq!(
            Template::parse(&format!("{base}count = 400\n[[round.slot]]\ncount = 200")),
            Err(TemplateError::TooBig)
        );
        assert!(matches!(
            Template::parse("nonsense = ["),
            Err(TemplateError::Parse(_))
        ));
        assert!(matches!(
            Template::parse(&format!("{base}colour = \"red\"")),
            Err(TemplateError::Parse(_))
        ));
        assert!(TemplateError::TooBig.to_string().contains("500"));
    }
}
