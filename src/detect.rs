//! Decides whether a typed word was written with the wrong keyboard layout.
//!
//! The approach and the thresholds are ported from ZabanYar's `core.py`
//! (https://github.com/shyguy2018/ZabanYar, MIT, (c) 2026 Behrooz Shayestepoor):
//!
//! 1. Look both readings of the keys up in a word-frequency list (Zipf scale,
//!    from wordfreq). A clearly more common reading in the other language wins.
//! 2. If neither reading is a known word, fall back to a character-bigram
//!    model built from the same lists, and only *suggest* (never auto-fix).
//!
//! Change from the original: Persian dictionary keys are stored with the
//! zero-width non-joiner removed and normalized, so `میخوام` also matches the
//! list entry `می‌خوام`.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use crate::mapping::{is_persian_char, normalize_fa};
use crate::userwords;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Lang {
    En,
    Fa,
}

impl Lang {
    pub fn other(self) -> Lang {
        match self {
            Lang::En => Lang::Fa,
            Lang::Fa => Lang::En,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Confidence {
    /// Safe to convert automatically.
    Sure,
    /// Probably wrong: show a hint and let the user decide.
    Unsure,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Verdict {
    pub confidence: Confidence,
    /// The word as it should have been typed.
    pub replacement: String,
    /// The language the user meant.
    pub target: Lang,
}

// ------------------------------------------------------------------ lexicon ---

pub struct Lexicon {
    freq: HashMap<String, f64>,
    bigram: HashMap<(char, char), f64>,
    totals: HashMap<char, f64>,
    alphabet: f64,
}

impl Lexicon {
    /// `data`: one `word<TAB>zipf` per line.
    fn parse(data: &str, persian: bool) -> Self {
        let mut freq: HashMap<String, f64> = HashMap::new();
        let mut counts: HashMap<(char, char), f64> = HashMap::new();
        let mut totals: HashMap<char, f64> = HashMap::new();

        for line in data.lines() {
            let Some((word, zipf)) = line.trim_end_matches('\r').split_once('\t') else { continue };
            let Ok(z) = zipf.parse::<f64>() else { continue };
            let key = if persian { fa_key(word) } else { word.to_string() };
            let entry = freq.entry(key.clone()).or_insert(0.0);
            if z > *entry {
                *entry = z;
            }
            // character bigram model, weighted by word frequency
            let weight = 10f64.powf(z - 3.0);
            let chars: Vec<char> = std::iter::once('^').chain(key.chars()).chain(std::iter::once('$')).collect();
            for pair in chars.windows(2) {
                *counts.entry((pair[0], pair[1])).or_insert(0.0) += weight;
                *totals.entry(pair[0]).or_insert(0.0) += weight;
            }
        }

        let alphabet = counts.keys().map(|(_, b)| *b).collect::<HashSet<_>>().len() as f64 + 1.0;
        let bigram = counts
            .iter()
            .map(|(k, v)| (*k, ((v + 0.01) / (totals[&k.0] + 0.01 * alphabet)).ln()))
            .collect();
        Lexicon { freq, bigram, totals, alphabet }
    }

    pub fn zipf(&self, word: &str) -> f64 {
        self.freq.get(word).copied().unwrap_or(0.0)
    }

    /// Average log-probability per character pair; higher = more "native looking".
    pub fn avg_logprob(&self, word: &str) -> f64 {
        let chars: Vec<char> = std::iter::once('^').chain(word.chars()).chain(std::iter::once('$')).collect();
        let sum: f64 = chars
            .windows(2)
            .map(|p| {
                self.bigram.get(&(p[0], p[1])).copied().unwrap_or_else(|| {
                    let total = self.totals.get(&p[0]).copied().unwrap_or(0.0);
                    (0.01 / (total + 0.01 * self.alphabet + 1.0)).ln()
                })
            })
            .sum();
        sum / (chars.len() - 1) as f64
    }
}

static LEXICONS: OnceLock<(Lexicon, Lexicon)> = OnceLock::new();

/// (English, Persian). Built on first use from the word lists embedded in the exe.
pub fn lexicons() -> &'static (Lexicon, Lexicon) {
    LEXICONS.get_or_init(|| {
        (
            Lexicon::parse(include_str!("../data/en.txt"), false),
            Lexicon::parse(include_str!("../data/fa.txt"), true),
        )
    })
}

// ----------------------------------------------------------------- cleaning ---

fn fa_key(s: &str) -> String {
    normalize_fa(s).replace('\u{200c}', "")
}

/// `[a-z]+('[a-z]+)?`
pub(crate) fn is_english_word(s: &str) -> bool {
    let part = |p: &str| !p.is_empty() && p.bytes().all(|b| b.is_ascii_lowercase());
    match s.split_once('\'') {
        Some((a, b)) => part(a) && part(b),
        None => part(s),
    }
}

/// English form used for lookup; empty if the keys can't form an English word.
fn clean_en(s: &str) -> String {
    let lower = s.to_lowercase();
    let trimmed = lower
        .trim_end_matches(|c| ".,!?:\"".contains(c))
        .trim_start_matches(|c| "\"(".contains(c));
    if is_english_word(trimmed) {
        trimmed.to_string()
    } else {
        String::new()
    }
}

fn clean_fa(s: &str) -> String {
    fa_key(s.trim_matches(|c| ".،؛:!؟?«»()[]-_/\\\"'".contains(c)))
}

/// The form a word is looked up by.
pub fn lookup_key(lang: Lang, word: &str) -> String {
    match lang {
        Lang::En => clean_en(word),
        Lang::Fa => clean_fa(word),
    }
}

/// How common a word the user added counts as: enough to win against a reading
/// that isn't a word, but not against a common word in the language typed.
const USER_WORD_ZIPF: f64 = 4.0;

fn known_zipf(lexicon: &Lexicon, lang: Lang, key: &str) -> f64 {
    if key.is_empty() {
        return 0.0;
    }
    let zipf = lexicon.zipf(key);
    if userwords::contains(lang, key) {
        zipf.max(USER_WORD_ZIPF)
    } else {
        zipf
    }
}

// ----------------------------------------------------------------- decision ---

/// * `en_str`  - the keys as they read in the English layout
/// * `fa_str`  - the same keys as they read in the Persian layout
/// * `typed`   - the layout that was actually active
/// * `context` - the language the previous word was corrected to (if recent):
///   inside a sentence typed in the wrong layout, trust that language more.
pub fn decide(en_str: &str, fa_str: &str, typed: Lang, context: Option<Lang>) -> Option<Verdict> {
    let (en_lex, fa_lex) = lexicons();
    let e = clean_en(en_str);
    let f = clean_fa(fa_str);
    let f_len = f.chars().count();

    if userwords::contains(typed, if typed == Lang::En { &e } else { &f }) {
        return None; // the user says this word is right
    }
    if typed == Lang::Fa && e.is_empty() {
        return None; // the keys don't make an English word
    }
    if f_len < 2 || (!e.is_empty() && e.len() < 2) || en_str.chars().any(|c| c.is_numeric()) {
        return None;
    }
    if typed == Lang::En && !f.chars().any(is_persian_char) {
        return None; // the keys don't even make Persian letters
    }

    let ez = known_zipf(en_lex, Lang::En, &e);
    let fz = known_zipf(fa_lex, Lang::Fa, &f);
    let (own, other, target, replacement) = match typed {
        Lang::En => (ez, fz, Lang::Fa, normalize_fa(fa_str)),
        Lang::Fa => (fz, ez, Lang::En, en_str.to_string()),
    };
    let verdict = |confidence| Some(Verdict { confidence, replacement: replacement.clone(), target });
    let gap = other - own;

    if context == Some(target) && other >= 3.5 && gap > 0.5 {
        return verdict(Confidence::Sure);
    }
    if own >= 4.2 && gap < 2.5 {
        return None; // a common word in the language that was typed
    }
    if other >= 2.8 && gap >= 1.8 {
        let sure = gap >= 2.5 && other >= 3.2;
        return verdict(if sure { Confidence::Sure } else { Confidence::Unsure });
    }

    // neither reading is a known word -> character-level guess (hint only)
    if own == 0.0 && other == 0.0 && f_len >= 4 {
        let (lp_own, lp_other) = match typed {
            Lang::En => (if e.is_empty() { -9.0 } else { en_lex.avg_logprob(&e) }, fa_lex.avg_logprob(&f)),
            Lang::Fa => (fa_lex.avg_logprob(&f), en_lex.avg_logprob(&e)),
        };
        if lp_other - lp_own >= 1.6 && lp_other > -3.6 {
            return verdict(Confidence::Unsure);
        }
    }
    None
}

// ------------------------------------------------------------------- phrases ---
// Not in ZabanYar: scoring the last few words together, so that short or
// ambiguous words inside a sentence typed in the wrong layout don't break the
// hint, and a single odd word doesn't trigger one.

/// How many recent words are considered together.
pub const PHRASE_WORDS: usize = 5;
/// Summed evidence needed when no single word is conclusive.
const PHRASE_MIN: f64 = 3.0;
/// A word scoring below this is "clearly typed right" and ends the phrase.
const CLEARLY_OWN: f64 = -1.0;

/// Evidence that a word was meant in the *other* language: positive = other,
/// negative = the typed language, roughly in Zipf units, clamped to +-4 and
/// down-weighted for words shorter than 3 letters.
pub fn evidence(en_str: &str, fa_str: &str, typed: Lang) -> f64 {
    if en_str.chars().any(|c| c.is_numeric()) {
        return -4.0; // numbers are never converted
    }
    let (en_lex, fa_lex) = lexicons();
    let e = clean_en(en_str);
    let f = clean_fa(fa_str);
    if userwords::contains(typed, if typed == Lang::En { &e } else { &f }) {
        return -4.0; // the user says this word is right
    }
    if typed == Lang::Fa && e.is_empty() {
        return -4.0; // the keys can't spell English
    }
    if typed == Lang::En && !f.is_empty() && !f.chars().any(is_persian_char) {
        return -4.0; // the keys can't spell Persian
    }
    let ez = known_zipf(en_lex, Lang::En, &e);
    let fz = known_zipf(fa_lex, Lang::Fa, &f);
    let (own, other) = match typed {
        Lang::En => (ez, fz),
        Lang::Fa => (fz, ez),
    };
    let f_len = f.chars().count();
    let raw = if own > 0.0 || other > 0.0 {
        other - own
    } else if !e.is_empty() && f_len >= 3 {
        let (lp_own, lp_other) = match typed {
            Lang::En => (en_lex.avg_logprob(&e), fa_lex.avg_logprob(&f)),
            Lang::Fa => (fa_lex.avg_logprob(&f), en_lex.avg_logprob(&e)),
        };
        (lp_other - lp_own) * 1.5
    } else {
        0.0
    };
    let weight = (f_len.max(e.len()) as f64 / 3.0).min(1.0);
    raw.clamp(-4.0, 4.0) * weight
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PhraseVerdict {
    /// Index into `words` of the first word to convert.
    pub first: usize,
    pub confidence: Confidence,
    pub target: Lang,
}

/// `words`: the last few words, oldest first, as (English reading, Persian
/// reading), all typed with the `typed` layout. The newest word is last.
pub fn decide_phrase(words: &[(&str, &str)], typed: Lang, context: Option<Lang>) -> Option<PhraseVerdict> {
    let last = words.len().checked_sub(1)?;
    let ev: Vec<f64> = words.iter().map(|(e, f)| evidence(e, f, typed)).collect();

    // the phrase: newest word plus the words before it that aren't clearly right
    let mut first = last;
    while first > 0 && ev[first - 1] > CLEARLY_OWN {
        first -= 1;
    }

    let confidence = match decide(words[last].0, words[last].1, typed, context) {
        Some(v) => v.confidence,
        None => {
            let sum: f64 = ev[first..].iter().sum();
            if last - first + 1 >= 2 && sum >= PHRASE_MIN && ev[last] > CLEARLY_OWN {
                Confidence::Unsure
            } else {
                return None;
            }
        }
    };
    Some(PhraseVerdict { first, confidence, target: typed.other() })
}

// -------------------------------------------------------------------- tests ---
// Single-word test cases ported from ZabanYar's tests/test_core.py.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{en_to_fa, fa_to_en};

    fn typed_en(w: &str, ctx: Option<Lang>) -> Option<Verdict> {
        decide(w, &en_to_fa(w), Lang::En, ctx)
    }

    fn typed_fa(w: &str, ctx: Option<Lang>) -> Option<Verdict> {
        decide(&fa_to_en(w), w, Lang::Fa, ctx)
    }

    #[test]
    fn persian_typed_on_english_layout() {
        for (typed, want) in [("sghl", "سلام"), ("o,fd", "خوبی"), ("lvsd", "مرسی"), ("llk,k", "ممنون"), ("ldo,hl", "میخوام")] {
            let v = typed_en(typed, None).unwrap_or_else(|| panic!("no verdict for {typed}"));
            assert_eq!((v.confidence, v.replacement.as_str(), v.target), (Confidence::Sure, want, Lang::Fa), "{typed}");
        }
    }

    #[test]
    fn english_typed_on_persian_layout() {
        for (typed, want) in [("غخع", "you"), ("ئثشد", "mean"), ("اثممخ", "hello"), ("حقهزث", "price"), ("فخئخققخص", "tomorrow")] {
            let v = typed_fa(typed, None).unwrap_or_else(|| panic!("no verdict for {typed}"));
            assert_eq!((v.confidence, v.replacement.as_str(), v.target), (Confidence::Sure, want, Lang::En), "{typed}");
        }
    }

    #[test]
    fn real_words_are_left_alone() {
        for w in ["hello", "the", "fluke", "marmonix", "ok", "meter", "don't"] {
            assert_eq!(typed_en(w, None), None, "{w}");
        }
        for w in ["سلام", "خوبی", "دستگاه", "آره", "اینو"] {
            assert_eq!(typed_fa(w, None), None, "{w}");
        }
    }

    #[test]
    fn sentence_context() {
        // 'شدی' is a real Persian word, but right after an English fix it means 'and'
        assert_eq!(typed_fa("شدی", None), None);
        assert_eq!(typed_fa("شدی", Some(Lang::En)).map(|v| v.replacement), Some("and".to_string()));
    }

    /// Splits a sentence typed on one layout into (en, fa) readings per word.
    fn sentence(typed: &str, lang: Lang) -> Vec<(String, String)> {
        typed
            .split(' ')
            .map(|w| match lang {
                Lang::En => (w.to_string(), en_to_fa(w)),
                Lang::Fa => (fa_to_en(w), w.to_string()),
            })
            .collect()
    }

    fn phrase(words: &[(String, String)], lang: Lang) -> Option<PhraseVerdict> {
        let refs: Vec<(&str, &str)> = words.iter().map(|(e, f)| (e.as_str(), f.as_str())).collect();
        decide_phrase(&refs, lang, None)
    }

    /// Feeds the sentence word by word, like typing, and returns the verdict
    /// after each word (looking at most PHRASE_WORDS words back).
    fn typing(s: &str, lang: Lang) -> Vec<Option<PhraseVerdict>> {
        let words = sentence(s, lang);
        (1..=words.len())
            .map(|n| phrase(&words[n.saturating_sub(PHRASE_WORDS)..n], lang))
            .collect()
    }

    #[test]
    fn correct_sentences_never_get_a_hint() {
        for s in ["i said hello to my friend", "is it ok to go in", "do as he is in a hd", "ok so we do it now"] {
            assert!(typing(s, Lang::En).iter().all(Option::is_none), "{s}");
        }
        for s in ["من با تو که از رو یه دو", "این کار رو امروز انجام میدم"] {
            assert!(typing(s, Lang::Fa).iter().all(Option::is_none), "{s}");
        }
    }

    #[test]
    fn short_words_add_up() {
        // تو رو دو: no single word is conclusive, together they are
        let v = typing("j, v, n,", Lang::En);
        assert_eq!(v[0], None);
        assert_eq!(v[2], Some(PhraseVerdict { first: 0, confidence: Confidence::Unsure, target: Lang::Fa }));
    }

    #[test]
    fn hint_covers_the_whole_wrong_sentence() {
        // the last word decides, the ambiguous ones before it are included
        let v = typing("di v, n, fh", Lang::En); // یه رو دو با
        assert_eq!(v[3].map(|p| (p.first, p.target)), Some((0, Lang::Fa)));
        let v = typing("هس هف خن فخ لخ", Lang::Fa); // is it ok to go
        assert_eq!(v[4].map(|p| (p.first, p.target)), Some((0, Lang::En)));
    }

    #[test]
    fn numbers_and_clearly_right_words_end_the_phrase() {
        let v = typing("sghl 123 lk", Lang::En);
        assert_eq!(v[2].map(|p| p.first), Some(2));
        let v = typing("hello sghl", Lang::En);
        assert_eq!(v[1].map(|p| p.first), Some(1));
    }

    #[test]
    fn a_user_word_gets_no_hint_in_its_own_language() {
        userwords::clear_for_test();
        assert!(typed_en("sghl", None).is_some());
        userwords::add(Lang::En, "sghl").unwrap();
        assert_eq!(typed_en("sghl", None), None);
        assert_eq!(typed_en("Sghl.", None), None);
        userwords::clear_for_test();
    }

    #[test]
    fn a_user_word_is_recognized_from_the_other_layout() {
        userwords::clear_for_test();
        let typed = en_to_fa("zqxvjk");
        assert_eq!(typed_fa(&typed, None).map(|v| v.replacement), None);
        userwords::add(Lang::En, "zqxvjk").unwrap();
        let v = typed_fa(&typed, None).unwrap();
        assert_eq!((v.confidence, v.replacement.as_str(), v.target), (Confidence::Sure, "zqxvjk", Lang::En));

        userwords::add(Lang::Fa, "تهرانپارسی").unwrap();
        let keys = fa_to_en("تهرانپارسی");
        assert_eq!(typed_en(&keys, None).map(|v| v.replacement).as_deref(), Some("تهرانپارسی"));
        userwords::clear_for_test();
    }

    #[test]
    fn a_user_word_ends_a_phrase() {
        userwords::clear_for_test();
        userwords::add(Lang::En, "fhfh").unwrap();
        let v = typing("fhfh sghl", Lang::En);
        assert_eq!(v[0], None);
        assert_eq!(v[1].map(|p| p.first), Some(1));
        userwords::clear_for_test();
    }

    /// Run with `cargo test explore -- --ignored --nocapture` to see the scores.
    #[test]
    #[ignore]
    fn explore() {
        for (s, lang) in [
            ("fh j, v, ;i hc", Lang::En),
            ("hc j, ;i fh lk", Lang::En),
            ("di v, n, fh", Lang::En),
            ("هس هف خن فخ لخ", Lang::Fa),
            ("i said hello to my friend", Lang::En),
            ("is it ok to go in", Lang::En),
            ("do as he is in a hd", Lang::En),
            ("ok so we do it now", Lang::En),
            ("من با تو که از رو یه دو", Lang::Fa),
            ("این کار رو امروز انجام میدم", Lang::Fa),
        ] {
            let words = sentence(s, lang);
            for (e, f) in &words {
                println!("{e:>10} {f:>10}  ev={:+.2}  single={:?}", evidence(e, f, lang), decide(e, f, lang, None).map(|v| v.confidence));
            }
            println!("  -> phrase {:?}\n", phrase(&words, lang));
        }
    }
}
