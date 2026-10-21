//! Text normalization and similarity measures (DESIGN.md §6). Deterministic and
//! locale-independent: no collation, no external services.

use serde::Deserialize;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::LazyLock;
use unicode_normalization::UnicodeNormalization;

#[derive(Debug, Deserialize)]
pub struct LanguageData {
    pub yes_no: WordList,
    pub numbers: NumberWords,
    pub years: YearData,
    pub stopwords: BTreeMap<String, Vec<String>>,
}

#[derive(Debug, Deserialize)]
pub struct WordList {
    pub words: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct NumberWords {
    pub words: Vec<String>,
    pub approximators: Vec<String>,
    pub units: Vec<String>,
    pub quantity_patterns: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct YearData {
    pub era_markers: Vec<String>,
    pub decade_suffixes: Vec<String>,
    pub prompt_patterns: Vec<String>,
}

pub static LANGUAGE: LazyLock<LanguageData> = LazyLock::new(|| {
    toml::from_str(include_str!("../../data/language.toml")).expect("data/language.toml is valid")
});

/// NFKC, lowercase, punctuation to spaces, whitespace collapsed. Diacritics are
/// kept: in Finnish and Swedish ä/ö/å are letters of their own.
pub fn normalize(s: &str) -> String {
    let lowered: String = s.nfkc().collect::<String>().to_lowercase();
    let mut out = String::with_capacity(lowered.len());
    let mut space = true;
    for ch in lowered.chars() {
        if ch.is_alphanumeric() {
            out.push(ch);
            space = false;
        } else if !space {
            out.push(' ');
            space = true;
        }
    }
    if out.ends_with(' ') {
        out.pop();
    }
    out
}

pub fn tokens(normalized: &str) -> Vec<&str> {
    normalized.split(' ').filter(|t| !t.is_empty()).collect()
}

/// Normalized text without the stopwords of `language` (or of every language
/// when the language has no list).
pub fn content_words(s: &str, language: &str) -> String {
    let n = normalize(s);
    let stop: BTreeSet<&str> = match LANGUAGE.stopwords.get(language) {
        Some(list) => list.iter().map(String::as_str).collect(),
        None => LANGUAGE
            .stopwords
            .values()
            .flatten()
            .map(String::as_str)
            .collect(),
    };
    let kept: Vec<&str> = tokens(&n)
        .into_iter()
        .filter(|t| !stop.contains(t))
        .collect();
    if kept.is_empty() {
        // A prompt made only of stopwords still has to compare as something.
        n
    } else {
        kept.join(" ")
    }
}

/// Character trigrams of each word, padded so word boundaries count.
pub fn trigrams(normalized: &str) -> BTreeSet<String> {
    let mut set = BTreeSet::new();
    for word in tokens(normalized) {
        let padded: Vec<char> = format!("  {word} ").chars().collect();
        for w in padded.windows(3) {
            set.insert(w.iter().collect());
        }
    }
    set
}

pub fn jaccard(a: &BTreeSet<String>, b: &BTreeSet<String>) -> f64 {
    if a.is_empty() && b.is_empty() {
        return 0.0;
    }
    let inter = a.intersection(b).count() as f64;
    let union = (a.len() + b.len()) as f64 - inter;
    inter / union
}

pub fn trigram_similarity(a: &str, b: &str) -> f64 {
    jaccard(&trigrams(a), &trigrams(b))
}

/// Levenshtein distance over chars.
pub fn edit_distance(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.iter().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

/// Lowercase hex SHA-256 of the parts joined by U+001F, the hash every
/// deterministic choice in the app is built on.
pub fn stable_hash(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    for (i, p) in parts.iter().enumerate() {
        if i > 0 {
            h.update([0x1f]);
        }
        h.update(p.as_bytes());
    }
    hex::encode(h.finalize())
}

/// Picks one of `n` options by hash, stable for the same key.
pub fn stable_pick(key: &[&str], n: usize) -> usize {
    assert!(n > 0, "stable_pick needs at least one option");
    let h = stable_hash(key);
    let v = u64::from_str_radix(&h[..16], 16).expect("hex prefix");
    (v % n as u64) as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalize_strips_punctuation_and_case_but_keeps_diacritics() {
        assert_eq!(
            normalize("  Mikä on Suomen PÄÄKAUPUNKI?! "),
            "mikä on suomen pääkaupunki"
        );
        assert_eq!(normalize("Rock'n'roll — \"live\""), "rock n roll live");
        assert_eq!(normalize("ﬁve"), "five"); // NFKC ligature
        assert_eq!(normalize("..."), "");
    }

    #[test]
    fn content_words_drop_stopwords_for_language_or_all() {
        assert_eq!(
            content_words("What is the capital of Finland?", "en"),
            "capital finland"
        );
        assert_eq!(
            content_words("Mikä on Suomen pääkaupunki?", "fi"),
            "suomen pääkaupunki"
        );
        // unknown language: every list applies
        assert_eq!(content_words("What is the capital?", "de"), "capital");
        // all stopwords: keep the text rather than nothing
        assert_eq!(content_words("What is that?", "en"), "what is that");
    }

    #[test]
    fn trigram_similarity_behaves() {
        assert_eq!(trigram_similarity("helsinki", "helsinki"), 1.0);
        assert_eq!(trigram_similarity("", ""), 0.0);
        assert_eq!(trigram_similarity("abc", "xyz"), 0.0);
        let s = trigram_similarity("capital finland", "capital of finland");
        assert!(s > 0.8, "{s}");
        let t = trigram_similarity("helsinki", "helsingfors");
        assert!(t > 0.2 && t < 0.6, "{t}");
    }

    #[test]
    fn edit_distance_classic_cases() {
        assert_eq!(edit_distance("kitten", "sitting"), 3);
        assert_eq!(edit_distance("", "abc"), 3);
        assert_eq!(edit_distance("pääkaupunki", "paakaupunki"), 2);
        assert_eq!(edit_distance("same", "same"), 0);
    }

    #[test]
    fn stable_hash_and_pick_are_deterministic() {
        let a = stable_hash(&["seed", "r1.s1", "42"]);
        assert_eq!(a, stable_hash(&["seed", "r1.s1", "42"]));
        assert_ne!(a, stable_hash(&["seed", "r1.s142"]));
        assert_eq!(a.len(), 64);
        let p = stable_pick(&["x"], 5);
        assert!(p < 5);
        assert_eq!(p, stable_pick(&["x"], 5));
    }

    #[test]
    fn language_data_parses() {
        assert!(LANGUAGE.yes_no.words.contains(&"kyllä".to_string()));
        assert!(!LANGUAGE.years.prompt_patterns.is_empty());
    }
}
