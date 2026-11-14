//! Static English <-> Persian key mapping and Persian normalization.
//!
//! The mapping table is ported from ZabanYar's `core.py`
//! (https://github.com/shyguy2018/ZabanYar, MIT, (c) 2026 Behrooz Shayestepoor).
//!
//! At runtime the app asks Windows (`ToUnicodeEx`) what each key produces in
//! the user's *own* installed layouts, so this table is only a fallback for
//! when a layout is missing, and it is what the unit tests use.

/// US key -> Persian character (Windows "Persian" layout).
const EN2FA: &[(char, char)] = &[
    ('q', 'ض'), ('w', 'ص'), ('e', 'ث'), ('r', 'ق'), ('t', 'ف'), ('y', 'غ'), ('u', 'ع'),
    ('i', 'ه'), ('o', 'خ'), ('p', 'ح'), ('[', 'ج'), (']', 'چ'), ('a', 'ش'), ('s', 'س'),
    ('d', 'ی'), ('f', 'ب'), ('g', 'ل'), ('h', 'ا'), ('j', 'ت'), ('k', 'ن'), ('l', 'م'),
    (';', 'ک'), ('\'', 'گ'), ('z', 'ظ'), ('x', 'ط'), ('c', 'ز'), ('v', 'ر'), ('b', 'ذ'),
    ('n', 'د'), ('m', 'پ'), (',', 'و'), ('\\', 'پ'), ('`', 'پ'),
    ('H', 'آ'), ('C', 'ژ'), ('M', 'ء'),
    ('1', '۱'), ('2', '۲'), ('3', '۳'), ('4', '۴'), ('5', '۵'),
    ('6', '۶'), ('7', '۷'), ('8', '۸'), ('9', '۹'), ('0', '۰'), ('?', '؟'),
];

/// Extra Persian -> US entries that win over the reversed table.
const FA2EN_EXTRA: &[(char, char)] = &[('پ', 'm'), ('ئ', 'm'), ('ي', 'd'), ('ك', ';'), ('ـ', 'j')];

const PERSIAN_CHARS: &str = "ابپتثجچحخدذرزژسشصضطظعغفقکگلمنوهیآءئأؤإةيك\u{200c}";

pub fn is_persian_char(c: char) -> bool {
    PERSIAN_CHARS.contains(c)
}

pub fn en_to_fa(s: &str) -> String {
    s.chars()
        .map(|ch| {
            let lookup = |c: char| EN2FA.iter().find(|(k, _)| *k == c).map(|(_, v)| *v);
            lookup(ch)
                // caps lock / shift: fall back to the lower-case key
                .or_else(|| if ch.is_alphabetic() { lookup(ch.to_ascii_lowercase()) } else { None })
                .unwrap_or(ch)
        })
        .collect()
}

pub fn fa_to_en(s: &str) -> String {
    s.chars()
        .map(|ch| {
            FA2EN_EXTRA
                .iter()
                .find(|(fa, _)| *fa == ch)
                .map(|(_, en)| *en)
                // first US key that produces this character
                .or_else(|| EN2FA.iter().find(|(_, fa)| *fa == ch).map(|(en, _)| *en))
                .unwrap_or(ch)
        })
        .collect()
}

/// Arabic Yeh/Kaf (and Alef Maksura) -> Persian Yeh/Keheh.
pub fn normalize_fa(s: &str) -> String {
    s.chars()
        .map(|c| match c {
            'ي' | 'ى' => 'ی',
            'ك' => 'ک',
            c => c,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        assert_eq!(en_to_fa("sghl"), "سلام");
        assert_eq!(fa_to_en("اثممخ"), "hello");
        assert_eq!(fa_to_en(&en_to_fa("ldo,hl")), "ldo,hl");
    }

    #[test]
    fn normalizes_arabic_letters() {
        assert_eq!(normalize_fa("كتابي"), "کتابی");
    }
}
