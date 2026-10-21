//! The typo guard (DESIGN.md §7.3): decides, without trusting the AI's own
//! label, whether an edit is small enough to apply without a human.

use super::model::{Content, Field, Patch};
use super::text::edit_distance;

pub const MAX_TOKEN_DISTANCE: usize = 2;
pub const MAX_CHANGED_TOKENS: usize = 5;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NotATypo {
    #[error("{0:?} may not be changed automatically")]
    Field(Field),
    #[error("words were added or removed in {0:?}")]
    WordCount(Field),
    #[error("more than {MAX_CHANGED_TOKENS} words changed in {0:?}")]
    TooManyWords(Field),
    #[error("'{0}' → '{1}' is more than a typo")]
    TooDifferent(String, String),
    #[error("a number changed: '{0}' → '{1}'")]
    Digits(String, String),
    #[error("a capitalized name changed: '{0}' → '{1}'")]
    Name(String, String),
    #[error("quoted text changed: '{0}' → '{1}'")]
    Quoted(String, String),
}

const QUOTES: [char; 8] = ['"', '“', '”', '„', '«', '»', '‘', '’'];

fn text_of(c: &Content, f: Field) -> &str {
    match f {
        Field::Prompt => &c.prompt,
        Field::Background => &c.background,
        Field::FunFact => &c.fun_fact,
        Field::HostNotes => &c.host_notes,
        _ => unreachable!("only text fields reach here"),
    }
}

/// Ok if every change from `old` to `new` passes the guard.
pub fn check(old: &Content, new: &Content) -> Result<(), NotATypo> {
    let patch = Patch::between(old, new);
    for f in patch.fields() {
        if !matches!(
            f,
            Field::Prompt | Field::Background | Field::FunFact | Field::HostNotes
        ) {
            return Err(NotATypo::Field(f));
        }
        check_text(f, text_of(old, f), text_of(new, f))?;
    }
    Ok(())
}

fn check_text(f: Field, old: &str, new: &str) -> Result<(), NotATypo> {
    let a: Vec<&str> = old.split_whitespace().collect();
    let b: Vec<&str> = new.split_whitespace().collect();
    if a.len() != b.len() {
        return Err(NotATypo::WordCount(f));
    }
    let mut in_quote = false;
    let mut changed = 0;
    for (i, (x, y)) in a.iter().zip(&b).enumerate() {
        let starts_quote = x.starts_with(QUOTES);
        let quoted = in_quote || starts_quote;
        let quote_marks = x.chars().filter(|c| QUOTES.contains(c)).count();
        if quote_marks % 2 == 1 {
            in_quote = !in_quote;
        }
        if x == y {
            continue;
        }
        changed += 1;
        if changed > MAX_CHANGED_TOKENS {
            return Err(NotATypo::TooManyWords(f));
        }
        let pair = || (x.to_string(), y.to_string());
        if x.chars().chain(y.chars()).any(|c| c.is_ascii_digit()) {
            let (p, q) = pair();
            return Err(NotATypo::Digits(p, q));
        }
        if quoted {
            let (p, q) = pair();
            return Err(NotATypo::Quoted(p, q));
        }
        let sentence_start = i == 0 || a[i - 1].ends_with(['.', '!', '?', ':']);
        let capitalized = x
            .chars()
            .find(|c| c.is_alphabetic())
            .is_some_and(char::is_uppercase);
        if capitalized && !sentence_start {
            let (p, q) = pair();
            return Err(NotATypo::Name(p, q));
        }
        let d = edit_distance(x, y);
        let len = x.chars().count();
        if d > MAX_TOKEN_DISTANCE || d * 10 > 3 * len {
            let (p, q) = pair();
            return Err(NotATypo::TooDifferent(p, q));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn c(prompt: &str) -> Content {
        Content::text(prompt, "Helsinki", "geography", "en")
    }

    #[test]
    fn simple_typo_passes() {
        assert_eq!(
            check(
                &c("Wich city is the captial of Finland?"),
                &c("Which city is the capital of Finland?")
            ),
            Ok(())
        );
        // sentence-initial capital word may be fixed
        assert_eq!(check(&c("Wich city?"), &c("Which city?")), Ok(()));
        // whitespace-only fix
        assert_eq!(check(&c("Which  city?"), &c("Which city?")), Ok(()));
    }

    #[test]
    fn typo_in_other_text_fields() {
        let mut a = c("Q?");
        a.fun_fact = "It was foundd in 1550.".into();
        let mut b = a.clone();
        b.fun_fact = "It was founded in 1550.".into();
        assert_eq!(check(&a, &b), Ok(()));
    }

    #[test]
    fn answer_and_other_fields_never_auto() {
        let a = c("Q?");
        let mut b = a.clone();
        b.answer = "Helsinky".into();
        assert_eq!(check(&a, &b), Err(NotATypo::Field(Field::Answer)));
        let mut b = a.clone();
        b.difficulty = 1;
        assert_eq!(check(&a, &b), Err(NotATypo::Field(Field::Difficulty)));
    }

    #[test]
    fn rejections() {
        assert_eq!(
            check(&c("Capital of Finland?"), &c("The capital of Finland?")),
            Err(NotATypo::WordCount(Field::Prompt))
        );
        assert!(matches!(
            check(&c("Built in 1550?"), &c("Built in 1551?")),
            Err(NotATypo::Digits(..))
        ));
        assert!(matches!(
            check(&c("Who is Sibelus here?"), &c("Who is Sibelius here?")),
            Err(NotATypo::Name(..))
        ));
        assert!(matches!(
            check(
                &c("Who sang \"Hello Dolly\" first?"),
                &c("Who sang \"Hallo Dolly\" first?")
            ),
            Err(NotATypo::Quoted(..))
        ));
        assert!(matches!(
            check(&c("Who wrote a book?"), &c("Who wrote a poem?")),
            Err(NotATypo::TooDifferent(..))
        ));
        // short words: one edit is over 30 %
        assert!(matches!(
            check(&c("is it at sea"), &c("is it on sea")),
            Err(NotATypo::TooDifferent(..))
        ));
        let many = check(
            &c("thhe quickk browwn foxx jumpedd overr"),
            &c("the quick brown fox jumped over"),
        );
        assert_eq!(many, Err(NotATypo::TooManyWords(Field::Prompt)));
    }

    #[test]
    fn quote_tracking_ends_after_closing_mark() {
        assert_eq!(
            check(
                &c("Who sang “Hello” and then went hoome?"),
                &c("Who sang “Hello” and then went home?")
            ),
            Ok(())
        );
    }

    #[test]
    fn errors_render() {
        assert!(
            NotATypo::Field(Field::Answer)
                .to_string()
                .contains("Answer")
        );
        assert!(
            NotATypo::TooManyWords(Field::Prompt)
                .to_string()
                .contains('5')
        );
    }
}
