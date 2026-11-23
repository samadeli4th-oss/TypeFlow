//! The user's own valid words, per language.
//!
//! Words get here two ways: added by hand in the Custom words window
//! (words.rs), or learned when the user rejects the same layout hint or
//! auto-fix more than three times.
//!
//! File: `%APPDATA%\TypeFlow\words.txt`, one `language<TAB>word` per line, with
//! a third `learned` column for learned words. Only accepted words are ever
//! written. The rejection counts stay in memory, so text that was merely
//! rejected a few times never reaches the disk.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::path::PathBuf;

use crate::detect::{self, Lang};
use crate::mapping::{is_persian_char, normalize_fa};

/// A word is learned on its 4th rejection ("more than 3 times").
pub const LEARN_AFTER: u8 = 4;
const MIN_LETTERS: usize = 2;
/// Two-letter key combinations are too often a real word in the other language.
const MIN_LEARNED_LETTERS: usize = 3;
const MAX_LETTERS: usize = 32;
/// Words being counted at once; the counts start over past this.
const MAX_TRACKED: usize = 500;
const ZWNJ: char = '\u{200c}';

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub lang: Lang,
    pub word: String,
    pub learned: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WordError {
    Empty,
    TooShort,
    TooLong,
    WrongLanguage,
    NotAWord,
    AlreadyAdded,
}

/// (word to show, lookup key), matching how detect.rs cleans what was typed.
fn parse_word(lang: Lang, input: &str) -> Result<(String, String), WordError> {
    let word = input.trim();
    if word.is_empty() {
        return Err(WordError::Empty);
    }
    let has_persian = word.chars().any(|c| c != ZWNJ && is_persian_char(c));
    let has_latin = word.chars().any(|c| c.is_ascii_alphabetic());
    if (lang == Lang::En && has_persian) || (lang == Lang::Fa && has_latin) {
        return Err(WordError::WrongLanguage);
    }
    let (shown, key) = match lang {
        Lang::En => (word.to_string(), word.to_lowercase()),
        Lang::Fa => {
            let shown = normalize_fa(word);
            let key = shown.replace(ZWNJ, "");
            (shown, key)
        }
    };
    let valid = match lang {
        Lang::En => detect::is_english_word(&key),
        Lang::Fa => !key.is_empty() && key.chars().all(is_persian_char),
    };
    if !valid {
        return Err(WordError::NotAWord);
    }
    let letters = key.chars().filter(|c| c.is_alphabetic()).count();
    if letters < MIN_LETTERS {
        return Err(WordError::TooShort);
    }
    if letters > MAX_LETTERS {
        return Err(WordError::TooLong);
    }
    Ok((shown, key))
}

/// What may be learned from typed text: plain words only. Anything with digits,
/// symbols or mixed case is skipped, because it could be a password typed into
/// a field Windows doesn't report as one.
fn learnable(lang: Lang, typed: &str) -> Option<(String, String)> {
    let word = match lang {
        Lang::En => typed.trim_matches(|c| ".,!?:\"(".contains(c)),
        Lang::Fa => typed.trim_matches(|c| ".،؛:!؟?«»()[]-_/\\\"'".contains(c)),
    };
    if word.chars().skip(1).any(char::is_uppercase) {
        return None;
    }
    let (shown, key) = parse_word(lang, word).ok()?;
    let letters = key.chars().filter(|c| c.is_alphabetic()).count();
    if letters < MIN_LEARNED_LETTERS {
        return None;
    }
    Some((if lang == Lang::En { key.clone() } else { shown }, key))
}

#[derive(Default)]
pub struct UserWords {
    en: BTreeMap<String, Entry>,
    fa: BTreeMap<String, Entry>,
    rejected: HashMap<(Lang, String), u8>,
}

impl UserWords {
    fn map(&self, lang: Lang) -> &BTreeMap<String, Entry> {
        match lang {
            Lang::En => &self.en,
            Lang::Fa => &self.fa,
        }
    }

    fn map_mut(&mut self, lang: Lang) -> &mut BTreeMap<String, Entry> {
        match lang {
            Lang::En => &mut self.en,
            Lang::Fa => &mut self.fa,
        }
    }

    /// Bad lines are skipped, so the file can be edited by hand.
    fn parse(text: &str) -> UserWords {
        let mut words = UserWords::default();
        for line in text.trim_start_matches('\u{feff}').lines() {
            let line = line.trim_end_matches('\r');
            if line.starts_with('#') {
                continue;
            }
            let mut columns = line.split('\t');
            let lang = match columns.next().map(str::trim) {
                Some("en") => Lang::En,
                Some("fa") => Lang::Fa,
                _ => continue,
            };
            let Some(Ok((word, key))) = columns.next().map(|w| parse_word(lang, w)) else { continue };
            let learned = columns.next().is_some_and(|c| c.trim() == "learned");
            words.map_mut(lang).insert(key, Entry { lang, word, learned });
        }
        words
    }

    fn to_text(&self) -> String {
        let mut text = String::from("# TypeFlow custom words: language<TAB>word, then <TAB>learned for learned words\n");
        for entry in self.en.values().chain(self.fa.values()) {
            let lang = if entry.lang == Lang::En { "en" } else { "fa" };
            let suffix = if entry.learned { "\tlearned" } else { "" };
            text.push_str(&format!("{lang}\t{}{suffix}\n", entry.word));
        }
        text
    }

    fn add(&mut self, lang: Lang, input: &str) -> Result<String, WordError> {
        let (word, key) = parse_word(lang, input)?;
        if self.map(lang).get(&key).is_some_and(|e| !e.learned) {
            return Err(WordError::AlreadyAdded);
        }
        self.rejected.remove(&(lang, key.clone()));
        self.map_mut(lang).insert(key, Entry { lang, word: word.clone(), learned: false });
        Ok(word)
    }

    fn remove(&mut self, lang: Lang, word: &str) -> bool {
        let Ok((_, key)) = parse_word(lang, word) else { return false };
        self.rejected.remove(&(lang, key.clone()));
        self.map_mut(lang).remove(&key).is_some()
    }

    /// Counts one rejection. True when this one made the word valid.
    fn reject(&mut self, lang: Lang, typed: &str) -> bool {
        let Some((word, key)) = learnable(lang, typed) else { return false };
        if self.map(lang).contains_key(&key) {
            return false;
        }
        if self.rejected.len() >= MAX_TRACKED {
            self.rejected.clear();
        }
        let id = (lang, key);
        let count = self.rejected.entry(id.clone()).or_insert(0);
        *count += 1;
        if *count < LEARN_AFTER {
            return false;
        }
        self.rejected.remove(&id);
        self.map_mut(lang).insert(id.1, Entry { lang, word, learned: true });
        true
    }

    /// The suggestion was right after all: start counting from zero.
    fn accepted(&mut self, lang: Lang, typed: &str) {
        if let Some((_, key)) = learnable(lang, typed) {
            self.rejected.remove(&(lang, key));
        }
    }
}

// ----------------------------------------------------------------- the list ---
// Only the main thread decides on words, so the list lives there.

thread_local! {
    static WORDS: RefCell<UserWords> = RefCell::new(UserWords::default());
}

/// `key` as cleaned by detect.rs.
pub fn contains(lang: Lang, key: &str) -> bool {
    !key.is_empty() && WORDS.with(|w| w.borrow().map(lang).contains_key(key))
}

pub fn entries() -> Vec<Entry> {
    WORDS.with(|w| {
        let w = w.borrow();
        w.en.values().chain(w.fa.values()).cloned().collect()
    })
}

pub fn count() -> usize {
    WORDS.with(|w| {
        let w = w.borrow();
        w.en.len() + w.fa.len()
    })
}

pub fn add(lang: Lang, input: &str) -> Result<String, WordError> {
    WORDS.with(|w| w.borrow_mut().add(lang, input))
}

pub fn remove(lang: Lang, word: &str) -> bool {
    WORDS.with(|w| w.borrow_mut().remove(lang, word))
}

pub fn reject(lang: Lang, typed: &str) -> bool {
    WORDS.with(|w| w.borrow_mut().reject(lang, typed))
}

pub fn accepted(lang: Lang, typed: &str) {
    WORDS.with(|w| w.borrow_mut().accepted(lang, typed));
}

/// Learning was turned off: drop every count.
pub fn forget_rejections() {
    WORDS.with(|w| w.borrow_mut().rejected.clear());
}

fn path() -> Option<PathBuf> {
    std::env::var_os("APPDATA").map(|dir| PathBuf::from(dir).join("TypeFlow").join("words.txt"))
}

pub fn load() {
    let Some(bytes) = path().and_then(|p| std::fs::read(p).ok()) else { return };
    let words = UserWords::parse(&String::from_utf8_lossy(&bytes));
    WORDS.with(|w| *w.borrow_mut() = words);
}

/// Writes a new file and swaps it in, so a failed save can't lose the list.
pub fn save() -> bool {
    let Some(file) = path() else { return false };
    let text = WORDS.with(|w| w.borrow().to_text());
    if let Some(dir) = file.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let temp = file.with_extension("tmp");
    let saved = std::fs::write(&temp, text).and_then(|()| std::fs::rename(&temp, &file));
    if let Err(e) = &saved {
        eprintln!("could not save {}: {e}", file.display());
        let _ = std::fs::remove_file(&temp);
    }
    saved.is_ok()
}

#[cfg(test)]
pub fn clear_for_test() {
    WORDS.with(|w| *w.borrow_mut() = UserWords::default());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn manual_words_are_checked_against_the_chosen_language() {
        let mut w = UserWords::default();
        assert_eq!(w.add(Lang::En, "  Marmonix "), Ok("Marmonix".to_string()));
        assert!(w.en.contains_key("marmonix"));
        assert_eq!(w.add(Lang::En, "marmonix"), Err(WordError::AlreadyAdded));
        assert_eq!(w.add(Lang::En, "سلام"), Err(WordError::WrongLanguage));
        assert_eq!(w.add(Lang::Fa, "hello"), Err(WordError::WrongLanguage));
        assert_eq!(w.add(Lang::En, "two words"), Err(WordError::NotAWord));
        assert_eq!(w.add(Lang::En, "abc123"), Err(WordError::NotAWord));
        assert_eq!(w.add(Lang::En, "a"), Err(WordError::TooShort));
        assert_eq!(w.add(Lang::En, "   "), Err(WordError::Empty));
        assert_eq!(w.add(Lang::En, &"a".repeat(33)), Err(WordError::TooLong));
    }

    #[test]
    fn persian_words_match_with_or_without_a_half_space() {
        let mut w = UserWords::default();
        w.add(Lang::Fa, "می\u{200c}خوامش").unwrap();
        assert!(w.fa.contains_key("میخوامش"));
        assert_eq!(w.add(Lang::Fa, "میخوامش"), Err(WordError::AlreadyAdded));
        w.add(Lang::Fa, "كتابش").unwrap(); // Arabic kaf
        assert!(w.fa.contains_key("کتابش"));
    }

    #[test]
    fn learned_on_the_fourth_rejection() {
        let mut w = UserWords::default();
        for _ in 1..LEARN_AFTER {
            assert!(!w.reject(Lang::En, "marmonix"));
        }
        assert!(!w.en.contains_key("marmonix"));
        assert!(w.reject(Lang::En, "marmonix"));
        assert!(w.en["marmonix"].learned);
        assert!(!w.reject(Lang::En, "marmonix"), "already known");
    }

    #[test]
    fn accepting_the_suggestion_restarts_the_count() {
        let mut w = UserWords::default();
        for _ in 1..LEARN_AFTER {
            w.reject(Lang::En, "sghl");
        }
        w.accepted(Lang::En, "sghl");
        assert!(!w.reject(Lang::En, "sghl"));
        assert!(!w.en.contains_key("sghl"));
    }

    #[test]
    fn password_like_text_is_never_learned() {
        let mut w = UserWords::default();
        for typed in ["Hunter2", "p@ssword", "hUnTeR", "abc123", "ab", "pass word"] {
            for _ in 0..LEARN_AFTER {
                assert!(!w.reject(Lang::En, typed), "{typed}");
            }
        }
        assert!(w.en.is_empty() && w.rejected.is_empty());
    }

    #[test]
    fn punctuation_and_a_capital_first_letter_count_as_the_same_word() {
        let mut w = UserWords::default();
        for typed in ["marmonix,", "Marmonix", "(marmonix", "marmonix."] {
            w.reject(Lang::En, typed);
        }
        assert_eq!(w.en["marmonix"].word, "marmonix");
    }

    #[test]
    fn file_round_trip_keeps_language_and_origin() {
        let mut w = UserWords::default();
        w.add(Lang::En, "Marmonix").unwrap();
        w.add(Lang::Fa, "تهران\u{200c}پارس").unwrap();
        for _ in 0..LEARN_AFTER {
            w.reject(Lang::En, "typeflow");
        }
        let back = UserWords::parse(&w.to_text());
        assert_eq!(back.en, w.en);
        assert_eq!(back.fa, w.fa);
        assert!(back.rejected.is_empty());
    }

    #[test]
    fn rejection_counts_are_not_written_to_the_file() {
        let mut w = UserWords::default();
        w.reject(Lang::En, "almostsecret");
        assert!(!w.to_text().contains("almostsecret"));
    }

    #[test]
    fn tolerates_hand_edits() {
        let w = UserWords::parse("\u{feff}# note\nen\tgood\nxx\tbad\nen\tnot valid\nfa\thello\nen\n");
        assert_eq!(w.en.len(), 1);
        assert!(w.fa.is_empty());
    }

    #[test]
    fn removing_a_word_also_clears_its_count() {
        let mut w = UserWords::default();
        w.add(Lang::En, "marmonix").unwrap();
        assert!(w.remove(Lang::En, "Marmonix"));
        assert!(!w.remove(Lang::En, "marmonix"));
        w.reject(Lang::En, "marmonix");
        w.remove(Lang::En, "marmonix");
        assert!(w.rejected.is_empty());
    }
}
