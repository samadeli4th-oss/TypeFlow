//! Spelling correction for English and Persian.
//!
//! Candidates come from TypeFlow's own word lists through a symmetric-delete
//! index and are ranked by how common the word is and how likely the typing
//! mistake is. The scores are normalized into a confidence for each suggestion.
//!
//! The word lists contain common misspellings (`teh`, `recieve`), so when
//! Windows has a spellchecker for the language it is asked whether a word is
//! real. Colloquial Persian (`میخوام`, `کتابخونه`) is in the lists and counts as
//! correct even though formal dictionaries reject it.
//!
//! Words are only held in memory, on a background thread, and never logged.

use std::collections::{HashMap, HashSet};

use crate::detect::Lang;
use crate::mapping;

pub const MAX_SUGGESTIONS: usize = 5;
/// Shorter words have too many neighbors to correct reliably.
pub const MIN_LETTERS: usize = 3;
const MAX_EDITS: usize = 2;
/// Words shorter than this are only corrected by a single edit.
const TWO_EDITS_FROM: usize = 5;
const PREFIX: usize = 7;
/// Zipf points one full typing mistake costs.
const EDIT_WEIGHT: f32 = 2.5;
/// Candidates checked for being real words before ranking stops.
const MAX_KEPT: usize = 12;
const MAX_CHECKED: usize = 30;
/// How much better the other layout's reading must score to silence spelling.
const OTHER_LAYOUT_MARGIN: f32 = 1.5;
/// A listed word this much rarer than a one-edit neighbor may be a misspelling.
const MUCH_MORE_COMMON: f32 = 1.5;
/// Listed words at or above these are trusted (Windows rejected / no Windows).
const TRUSTED_WHEN_REJECTED: f32 = 4.0;
const TRUSTED_WITHOUT_CHECKER: f32 = 3.2;

const ROWS: [(&str, f32); 3] = [("qwertyuiop[]", 0.0), ("asdfghjkl;'", 0.25), ("zxcvbnm,./", 0.75)];
/// Persian letters that sound alike and are commonly swapped.
const SOUND_ALIKE: [&str; 6] = ["ذزضظ", "ثسص", "تط", "غق", "هح", "اع"];

#[derive(Debug, Clone, PartialEq)]
pub struct Suggestion {
    pub word: String,
    /// 0..=1. Suggestions are sorted by this, highest first.
    pub confidence: f32,
}

/// Asks whether a word is spelled correctly. `None` = can't tell.
pub trait Checker {
    fn is_correct(&mut self, lang: Lang, word: &str) -> Option<bool>;
}

/// Behaves like a machine without a Windows spellchecker.
#[cfg(test)]
pub struct NoChecker;

#[cfg(test)]
impl Checker for NoChecker {
    fn is_correct(&mut self, _: Lang, _: &str) -> Option<bool> {
        None
    }
}

// ----------------------------------------------------------- typing mistakes ---

fn key_position(c: char) -> Option<(f32, f32)> {
    ROWS.iter().enumerate().find_map(|(row, (keys, offset))| {
        keys.chars().position(|k| k == c).map(|i| (row as f32, i as f32 + offset))
    })
}

fn physical_key(c: char, lang: Lang) -> char {
    match lang {
        Lang::En => c,
        Lang::Fa => mapping::fa_to_en(&c.to_string()).chars().next().unwrap_or(c),
    }
}

fn keys_adjacent(a: char, b: char) -> bool {
    match (key_position(a), key_position(b)) {
        (Some(x), Some(y)) => a != b && ((x.0 - y.0).powi(2) + (x.1 - y.1).powi(2)).sqrt() <= 1.3,
        _ => false,
    }
}

fn substitution_cost(a: char, b: char, lang: Lang) -> f32 {
    if a == b {
        0.0
    } else if keys_adjacent(physical_key(a, lang), physical_key(b, lang))
        || (lang == Lang::Fa && SOUND_ALIKE.iter().any(|g| g.contains(a) && g.contains(b)))
    {
        0.6
    } else {
        1.0
    }
}

/// Adding or dropping a doubled letter is an easy mistake, and so is the
/// silent و in Persian خوا (خواهش written as خاهش).
fn indel_cost(word: &[char], i: usize, lang: Lang) -> f32 {
    if lang == Lang::Fa && word[i] == 'و' && i > 0 && word[i - 1] == 'خ' && word.get(i + 1) == Some(&'ا') {
        0.3
    } else if (i > 0 && word[i] == word[i - 1]) || (i + 1 < word.len() && word[i] == word[i + 1]) {
        0.5
    } else {
        1.0
    }
}

fn weighted_distance(a: &[char], b: &[char], lang: Lang) -> f32 {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0f32; m + 1]; n + 1];
    for i in 1..=n {
        d[i][0] = d[i - 1][0] + indel_cost(a, i - 1, lang);
    }
    for j in 1..=m {
        d[0][j] = d[0][j - 1] + indel_cost(b, j - 1, lang);
    }
    for i in 1..=n {
        for j in 1..=m {
            let mut v = (d[i - 1][j] + indel_cost(a, i - 1, lang))
                .min(d[i][j - 1] + indel_cost(b, j - 1, lang))
                .min(d[i - 1][j - 1] + substitution_cost(a[i - 1], b[j - 1], lang));
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 0.6);
            }
            d[i][j] = v;
        }
    }
    d[n][m]
}

/// Optimal string alignment: edits, counting a swap of neighbors as one.
fn edit_distance(a: &[char], b: &[char]) -> usize {
    let (n, m) = (a.len(), b.len());
    let mut d = vec![vec![0usize; m + 1]; n + 1];
    for (i, row) in d.iter_mut().enumerate() {
        row[0] = i;
    }
    for j in 0..=m {
        d[0][j] = j;
    }
    for i in 1..=n {
        for j in 1..=m {
            let cost = (a[i - 1] != b[j - 1]) as usize;
            let mut v = (d[i - 1][j] + 1).min(d[i][j - 1] + 1).min(d[i - 1][j - 1] + cost);
            if i > 1 && j > 1 && a[i - 1] == b[j - 2] && a[i - 2] == b[j - 1] {
                v = v.min(d[i - 2][j - 2] + 1);
            }
            d[i][j] = v;
        }
    }
    d[n][m]
}

// --------------------------------------------------------------------- index ---

fn hash(word: &[char]) -> u32 {
    word.iter().fold(0x811c_9dc5u32, |h, c| (h ^ *c as u32).wrapping_mul(16_777_619))
}

fn deletes(word: &[char], left: usize, out: &mut HashSet<u32>) {
    out.insert(hash(word));
    if left == 0 || word.len() <= 1 {
        return;
    }
    for i in 0..word.len() {
        let mut shorter = word.to_vec();
        shorter.remove(i);
        deletes(&shorter, left - 1, out);
    }
}

/// The form words are compared in.
fn key_of(lang: Lang, word: &str) -> String {
    match lang {
        Lang::En => word.to_lowercase(),
        Lang::Fa => mapping::normalize_fa(word).replace('\u{200c}', ""),
    }
}

#[derive(Debug, Clone, Copy)]
struct Candidate {
    id: u32,
    edits: usize,
    score: f32,
}

struct Index {
    lang: Lang,
    /// Sorted, so lookups are a binary search.
    keys: Vec<Box<str>>,
    /// The best-known way to write each key (with its half-spaces).
    surface: Vec<Box<str>>,
    zipf: Vec<f32>,
    /// (hash of a deletion, word id), sorted.
    pairs: Vec<(u32, u32)>,
}

impl Index {
    fn build(data: &str, lang: Lang) -> Index {
        let mut best: HashMap<String, (f32, String)> = HashMap::new();
        for line in data.lines() {
            let Some((word, zipf)) = line.trim_end_matches('\r').split_once('\t') else { continue };
            let Ok(zipf) = zipf.parse::<f32>() else { continue };
            let surface = if lang == Lang::Fa { mapping::normalize_fa(word) } else { word.to_string() };
            let entry = best.entry(key_of(lang, word)).or_insert((0.0, String::new()));
            if zipf > entry.0 {
                *entry = (zipf, surface);
            }
        }
        let mut rows: Vec<(String, (f32, String))> = best.into_iter().collect();
        rows.sort_by(|a, b| a.0.cmp(&b.0));

        let mut pairs = Vec::new();
        let mut seen = HashSet::new();
        for (id, (key, _)) in rows.iter().enumerate() {
            let chars: Vec<char> = key.chars().collect();
            seen.clear();
            deletes(&chars[..chars.len().min(PREFIX)], MAX_EDITS, &mut seen);
            pairs.extend(seen.iter().map(|h| (*h, id as u32)));
        }
        pairs.sort_unstable();
        pairs.shrink_to_fit();

        let zipf = rows.iter().map(|r| r.1 .0).collect();
        let (mut keys, mut surface) = (Vec::with_capacity(rows.len()), Vec::with_capacity(rows.len()));
        for (key, (_, written)) in rows {
            keys.push(key.into_boxed_str());
            surface.push(written.into_boxed_str());
        }
        Index { lang, keys, surface, zipf, pairs }
    }

    fn zipf_of(&self, key: &str) -> f32 {
        self.keys.binary_search_by(|k| k.as_ref().cmp(key)).map_or(0.0, |i| self.zipf[i])
    }

    /// Words within reach of `word`, best first.
    fn candidates(&self, word: &[char]) -> Vec<Candidate> {
        let limit = if word.len() < TWO_EDITS_FROM { 1 } else { MAX_EDITS };
        let mut hashes = HashSet::new();
        deletes(&word[..word.len().min(PREFIX)], MAX_EDITS, &mut hashes);
        let mut ids: Vec<u32> = Vec::new();
        for h in hashes {
            let start = self.pairs.partition_point(|p| p.0 < h);
            ids.extend(self.pairs[start..].iter().take_while(|p| p.0 == h).map(|p| p.1));
        }
        ids.sort_unstable();
        ids.dedup();
        let mut found: Vec<Candidate> = ids
            .into_iter()
            .filter_map(|id| {
                let other: Vec<char> = self.keys[id as usize].chars().collect();
                if other.len().abs_diff(word.len()) > limit {
                    return None;
                }
                let edits = edit_distance(word, &other);
                if edits == 0 || edits > limit {
                    return None;
                }
                let cost = weighted_distance(word, &other, self.lang);
                Some(Candidate { id, edits, score: self.zipf[id as usize] - EDIT_WEIGHT * cost })
            })
            .collect();
        found.sort_by(|a, b| b.score.total_cmp(&a.score).then(a.id.cmp(&b.id)));
        found
    }

    /// A listed word that sits within `edits` of a far more common word.
    fn has_common_neighbor(&self, zipf: f32, candidates: &[Candidate], edits: usize) -> bool {
        candidates.iter().any(|c| c.edits <= edits && self.zipf[c.id as usize] >= zipf + MUCH_MORE_COMMON)
    }
}

// -------------------------------------------------------------------- engine ---

pub struct Engine {
    en: Index,
    fa: Index,
}

impl Engine {
    pub fn build() -> Engine {
        Engine {
            en: Index::build(include_str!("../data/en.txt"), Lang::En),
            fa: Index::build(include_str!("../data/fa.txt"), Lang::Fa),
        }
    }

    fn index(&self, lang: Lang) -> &Index {
        match lang {
            Lang::En => &self.en,
            Lang::Fa => &self.fa,
        }
    }

    /// Corrections for `typed`, best first; empty if it looks right.
    /// `other_reading` is what the same keys spell in the other layout.
    pub fn suggest(&self, lang: Lang, typed: &str, other_reading: &str, checker: &mut dyn Checker) -> Vec<Suggestion> {
        let index = self.index(lang);
        let key = key_of(lang, typed);
        let chars: Vec<char> = key.chars().collect();
        if chars.len() < MIN_LETTERS {
            return Vec::new();
        }
        let candidates = index.candidates(&chars);
        let check_as = if lang == Lang::En { key.as_str() } else { typed };
        if is_correct(index, &key, check_as, &candidates, checker) {
            return Vec::new();
        }
        let Some(best) = candidates.first() else { return Vec::new() };
        // It may be a word typed in the wrong layout, with a typo: that's the
        // layout detector's job, so spelling stays out of it.
        if self.other_layout_wins(lang, best.score, other_reading) {
            return Vec::new();
        }

        let mut kept: Vec<Candidate> = Vec::new();
        for c in candidates.iter().take(MAX_CHECKED) {
            if is_real_word(index, *c, checker) {
                kept.push(*c);
                if kept.len() == MAX_KEPT {
                    break;
                }
            }
        }
        let Some(top) = kept.first().map(|c| c.score) else { return Vec::new() };
        let total: f32 = kept.iter().map(|c| 10f32.powf(c.score - top)).sum();
        let capitalized = typed.chars().next().is_some_and(char::is_uppercase);
        kept.iter()
            .map(|c| Suggestion {
                word: with_case(&index.surface[c.id as usize], capitalized),
                confidence: 10f32.powf(c.score - top) / total,
            })
            .take(MAX_SUGGESTIONS)
            .collect()
    }

    fn other_layout_wins(&self, lang: Lang, own_best: f32, reading: &str) -> bool {
        let other = lang.other();
        let index = self.index(other);
        let key = key_of(other, reading);
        let usable = match other {
            Lang::En => key.chars().all(|c| c.is_ascii_lowercase() || c == '\''),
            Lang::Fa => key.chars().all(mapping::is_persian_char),
        };
        let chars: Vec<char> = key.chars().collect();
        if !usable || chars.len() < 2 {
            return false;
        }
        if index.zipf_of(&key) >= own_best + OTHER_LAYOUT_MARGIN {
            return true;
        }
        index.candidates(&chars).first().is_some_and(|c| c.edits == 1 && c.score >= own_best + OTHER_LAYOUT_MARGIN)
    }
}

fn is_correct(index: &Index, key: &str, word: &str, candidates: &[Candidate], checker: &mut dyn Checker) -> bool {
    let listed = index.zipf_of(key);
    if index.lang == Lang::Fa {
        return listed > 0.0 || checker.is_correct(Lang::Fa, word) == Some(true);
    }
    match checker.is_correct(Lang::En, word) {
        Some(true) => true,
        Some(false) => {
            listed >= TRUSTED_WHEN_REJECTED || (listed > 0.0 && !index.has_common_neighbor(listed, candidates, MAX_EDITS))
        }
        None => listed >= TRUSTED_WITHOUT_CHECKER || (listed > 0.0 && !index.has_common_neighbor(listed, candidates, 1)),
    }
}

/// Never suggest a misspelling that happens to be in the word list.
fn is_real_word(index: &Index, candidate: Candidate, checker: &mut dyn Checker) -> bool {
    if index.lang == Lang::Fa {
        return true;
    }
    let id = candidate.id as usize;
    match checker.is_correct(Lang::En, &index.keys[id]) {
        Some(answer) => answer,
        None => {
            let zipf = index.zipf[id];
            if zipf >= TRUSTED_WITHOUT_CHECKER {
                return true;
            }
            let chars: Vec<char> = index.keys[id].chars().collect();
            !index.has_common_neighbor(zipf, &index.candidates(&chars), 1)
        }
    }
}

fn with_case(word: &str, capitalized: bool) -> String {
    let mut chars = word.chars();
    match chars.next() {
        Some(first) if capitalized => first.to_uppercase().chain(chars).collect(),
        _ => word.to_string(),
    }
}

// ------------------------------------------------------------ windows service ---

#[cfg(windows)]
pub use service::Service;

#[cfg(windows)]
mod service {
    use std::ffi::c_void;
    use std::sync::mpsc::{channel, Receiver, Sender};

    use windows::core::PCWSTR;
    use windows::Win32::Foundation::{HWND, LPARAM, S_OK, WPARAM};
    use windows::Win32::Globalization::{ISpellChecker, ISpellCheckerFactory, SpellCheckerFactory};
    use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
    use windows::Win32::UI::WindowsAndMessaging::PostMessageW;

    use super::{Checker, Engine, Lang, Suggestion};

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain([0]).collect()
    }

    /// The spellcheckers Windows has for each language, if any.
    struct SystemChecker {
        en: Option<ISpellChecker>,
        fa: Option<ISpellChecker>,
    }

    impl SystemChecker {
        fn new() -> SystemChecker {
            let factory: Option<ISpellCheckerFactory> =
                unsafe { CoCreateInstance(&SpellCheckerFactory, None, CLSCTX_INPROC_SERVER) }.ok();
            let open = |tags: &[&str]| -> Option<ISpellChecker> {
                let factory = factory.as_ref()?;
                tags.iter().find_map(|tag| unsafe {
                    let tag = wide(tag);
                    let supported = factory.IsSupported(PCWSTR(tag.as_ptr())).ok()?;
                    if !supported.as_bool() {
                        return None;
                    }
                    factory.CreateSpellChecker(PCWSTR(tag.as_ptr())).ok()
                })
            };
            SystemChecker { en: open(&["en-US", "en-GB", "en-CA", "en-AU"]), fa: open(&["fa-IR"]) }
        }
    }

    impl Checker for SystemChecker {
        fn is_correct(&mut self, lang: Lang, word: &str) -> Option<bool> {
            let checker = match lang {
                Lang::En => self.en.as_ref()?,
                Lang::Fa => self.fa.as_ref()?,
            };
            let text = wide(word);
            unsafe {
                let errors = checker.Check(PCWSTR(text.as_ptr())).ok()?;
                let mut error = None;
                let result = errors.Next(&mut error);
                if result.is_err() {
                    return None;
                }
                Some(result != S_OK || error.is_none())
            }
        }
    }

    enum Job {
        Wake,
        Unload,
        Check { id: u64, lang: Lang, word: String, other: String },
    }

    pub struct Reply {
        pub id: u64,
        pub suggestions: Vec<Suggestion>,
    }

    /// The background thread that does all spelling work, so typing is never
    /// held up. Builds its index on first use and frees it when switched off.
    pub struct Service {
        jobs: Sender<Job>,
        replies: Receiver<Reply>,
    }

    impl Service {
        /// `message` is posted to `window` whenever a reply is ready.
        pub fn start(window: HWND, message: u32) -> Service {
            let (jobs, job_queue) = channel();
            let (reply_out, replies) = channel();
            let target = window.0 as isize;
            let _ = std::thread::Builder::new()
                .name("typeflow-spell".into())
                .spawn(move || worker(job_queue, reply_out, target, message));
            Service { jobs, replies }
        }

        pub fn wake(&self) {
            let _ = self.jobs.send(Job::Wake);
        }

        pub fn unload(&self) {
            let _ = self.jobs.send(Job::Unload);
        }

        pub fn check(&self, id: u64, lang: Lang, word: String, other: String) {
            let _ = self.jobs.send(Job::Check { id, lang, word, other });
        }

        pub fn next_reply(&self) -> Option<Reply> {
            self.replies.try_recv().ok()
        }
    }

    fn worker(jobs: Receiver<Job>, replies: Sender<Reply>, window: isize, message: u32) {
        let _ = unsafe { CoInitializeEx(None, COINIT_MULTITHREADED) };
        let mut engine: Option<Engine> = None;
        let mut checker: Option<SystemChecker> = None;
        for job in jobs {
            match job {
                Job::Wake => {
                    engine.get_or_insert_with(Engine::build);
                    checker.get_or_insert_with(SystemChecker::new);
                }
                Job::Unload => {
                    engine = None;
                    checker = None;
                }
                Job::Check { id, lang, word, other } => {
                    let engine = engine.get_or_insert_with(Engine::build);
                    let checker = checker.get_or_insert_with(SystemChecker::new);
                    let suggestions = engine.suggest(lang, &word, &other, checker);
                    if replies.send(Reply { id, suggestions }).is_ok() {
                        let hwnd = HWND(window as *mut c_void);
                        let _ = unsafe { PostMessageW(hwnd, message, WPARAM(0), LPARAM(0)) };
                    }
                }
            }
        }
    }
}

// -------------------------------------------------------------------- tests ---
#[cfg(test)]
mod tests {
    use super::*;
    use crate::mapping::{en_to_fa, fa_to_en};
    use std::sync::OnceLock;

    fn engine() -> &'static Engine {
        static ENGINE: OnceLock<Engine> = OnceLock::new();
        ENGINE.get_or_init(Engine::build)
    }

    fn en(word: &str) -> Vec<Suggestion> {
        engine().suggest(Lang::En, word, &en_to_fa(word), &mut NoChecker)
    }

    fn fa(word: &str) -> Vec<Suggestion> {
        engine().suggest(Lang::Fa, word, &fa_to_en(word), &mut NoChecker)
    }

    #[test]
    fn corrects_common_english_misspellings() {
        for (typo, want) in [("teh", "the"), ("recieve", "receive"), ("definately", "definitely"),
            ("seperate", "separate"), ("goverment", "government"), ("helllo", "hello")] {
            let list = en(typo);
            assert!(list.iter().any(|s| s.word == want), "{typo}: {list:?}");
        }
    }

    #[test]
    fn corrects_common_persian_misspellings() {
        for (typo, want) in [("خاهش", "خواهش"), ("دانشگا", "دانشگاه"), ("ممنوون", "ممنون"),
            ("اطلاغات", "اطلاعات"), ("پیشنحاد", "پیشنهاد"), ("صحبط", "صحبت")] {
            let list = fa(typo);
            assert!(list.iter().any(|s| s.word == want), "{typo}: {list:?}");
        }
    }

    #[test]
    fn the_silent_vav_ranks_first() {
        for (typo, want) in [("خاهش", "خواهش"), ("خاستن", "خواستن"), ("خاهر", "خواهر")] {
            assert_eq!(fa(typo).first().map(|s| s.word.as_str()), Some(want), "{typo}");
        }
    }

    #[test]
    fn offers_alternatives_even_when_one_is_very_likely() {
        let list = en("teh");
        assert_eq!(list[0].word, "the");
        assert!(list.len() > 1, "{list:?}");
    }

    #[test]
    fn sorted_by_confidence() {
        for list in [en("recieve"), fa("خاهش")] {
            assert!(!list.is_empty() && list.len() <= MAX_SUGGESTIONS);
            assert!(list.windows(2).all(|p| p[0].confidence >= p[1].confidence), "{list:?}");
            assert!(list.iter().map(|s| s.confidence).sum::<f32>() <= 1.001);
        }
    }

    #[test]
    fn correct_words_are_left_alone() {
        for w in ["hello", "the", "because", "receive", "ten", "form"] {
            assert_eq!(en(w), vec![], "{w}");
        }
        for w in ["سلام", "خوبی", "دانشگاه", "میخوام", "می\u{200c}خواهم"] {
            assert_eq!(fa(w), vec![], "{w}");
        }
    }

    #[test]
    fn never_suggests_a_listed_misspelling() {
        for typo in ["recive", "definatly", "seperete"] {
            let list = en(typo);
            for bad in ["recieve", "definately", "seperate"] {
                assert!(list.iter().all(|s| s.word != bad), "{typo}: {list:?}");
            }
        }
    }

    #[test]
    fn keeps_a_capital_letter() {
        assert_eq!(en("Recieve").first().map(|s| s.word.as_str()), Some("Receive"));
    }

    #[test]
    fn leaves_wrong_layout_typos_to_the_detector() {
        // سلام typed on the English layout with one wrong key
        assert_eq!(en("sghk"), vec![]);
    }

    struct Rejects(&'static [&'static str]);
    impl Checker for Rejects {
        fn is_correct(&mut self, _: Lang, word: &str) -> Option<bool> {
            Some(!self.0.contains(&word))
        }
    }

    #[test]
    fn the_windows_checker_decides_for_english() {
        let list = engine().suggest(Lang::En, "teh", &en_to_fa("teh"), &mut Rejects(&["teh"]));
        assert_eq!(list.first().map(|s| s.word.as_str()), Some("the"));
        // two edits away, and in the word list as if it were real
        let list = engine().suggest(Lang::En, "tommorow", &en_to_fa("tommorow"), &mut Rejects(&["tommorow"]));
        assert!(list.iter().any(|s| s.word == "tomorrow"), "{list:?}");
        assert_eq!(engine().suggest(Lang::En, "zzxqv", "", &mut Rejects(&[])), vec![]);
    }

    #[test]
    fn colloquial_persian_stays_correct_even_if_windows_rejects_it() {
        struct RejectAll;
        impl Checker for RejectAll {
            fn is_correct(&mut self, _: Lang, _: &str) -> Option<bool> {
                Some(false)
            }
        }
        let word = "میخوام";
        assert_eq!(engine().suggest(Lang::Fa, word, &fa_to_en(word), &mut RejectAll), vec![]);
    }
}
