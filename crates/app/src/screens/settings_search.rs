//! Search inside the settings: the words typed on the on-screen keyboard are looked for in each
//! option's label, both as shown in the interface language and in English (the keyboard has Latin
//! letters only, and a viewer may know an option by its English name), ignoring capitals and
//! accents.
//!
//! The keyboard is a screen of its own above the settings: it leaves the typed words here
//! ([`submit`]) as it closes, and the settings screen takes them ([`take_query`]) when it is
//! uncovered. A result on a sub-page (segment markers) opens that page with the focus on the
//! option, through [`set_start`] / [`take_start`].

use crate::host::SettingsPage;
use std::sync::Mutex;

/// Words typed on the keyboard, waiting for the settings screen below it.
static QUERY: Mutex<Option<String>> = Mutex::new(None);
/// The row a settings sub-page about to open puts the focus on.
static START: Mutex<Option<(SettingsPage, usize)>> = Mutex::new(None);

/// The keyboard closed with these words (blank: the search is cleared).
pub fn submit(query: &str) {
    if let Ok(mut q) = QUERY.lock() {
        *q = Some(query.trim().to_string());
    }
}

/// The words the keyboard left, once.
pub fn take_query() -> Option<String> {
    QUERY.lock().ok().and_then(|mut q| q.take())
}

/// The sub-page `page` opening next starts with the focus on its row `row`.
pub fn set_start(page: SettingsPage, row: usize) {
    if let Ok(mut s) = START.lock() {
        *s = Some((page, row));
    }
}

/// The row a newly opened `page` starts on, when a search result opened it.
pub fn take_start(page: SettingsPage) -> Option<usize> {
    let mut s = START.lock().ok()?;
    match *s {
        Some((p, row)) if p == page => {
            *s = None;
            Some(row)
        }
        _ => None,
    }
}

/// `s` in small letters without accents, so "Hang", "háng" and "HÁNG" read the same.
pub fn fold(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars().flat_map(char::to_lowercase) {
        match c {
            'á' | 'à' | 'â' | 'ä' | 'ã' | 'å' | 'ā' => out.push('a'),
            'é' | 'è' | 'ê' | 'ë' | 'ě' | 'ē' => out.push('e'),
            'í' | 'ì' | 'î' | 'ï' | 'ī' => out.push('i'),
            'ó' | 'ò' | 'ô' | 'ö' | 'õ' | 'ő' | 'ø' | 'ō' => out.push('o'),
            'ú' | 'ù' | 'û' | 'ü' | 'ű' | 'ů' | 'ū' => out.push('u'),
            'ý' | 'ÿ' => out.push('y'),
            'ñ' | 'ń' | 'ň' => out.push('n'),
            'ç' | 'ć' | 'č' => out.push('c'),
            'š' | 'ś' => out.push('s'),
            'ž' | 'ź' | 'ż' => out.push('z'),
            'ř' => out.push('r'),
            'ł' => out.push('l'),
            'ß' => out.push_str("ss"),
            'œ' => out.push_str("oe"),
            'æ' => out.push_str("ae"),
            'ё' => out.push('е'),
            'й' => out.push('и'),
            // Typographic apostrophes and dashes as typed on the keyboard.
            '\u{2019}' | '\u{2018}' => out.push('\''),
            '\u{2013}' | '\u{2014}' => out.push('-'),
            c => out.push(c),
        }
    }
    out
}

/// The query's words, folded ([`fold`]); none when it is blank.
pub fn words(query: &str) -> Vec<String> {
    fold(query).split_whitespace().map(str::to_string).collect()
}

/// Every word appears in the label as shown or in its English original (in any order, each
/// anywhere in the label: "speed default" finds "Default speed").
pub fn matches(words: &[String], shown: &str, english: &str) -> bool {
    if words.is_empty() {
        return false;
    }
    let all_in = |label: &str| {
        let l = fold(label);
        words.iter().all(|w| l.contains(w.as_str()))
    };
    all_in(shown) || (english != shown && all_in(english))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn folding_drops_capitals_and_accents() {
        assert_eq!(fold("Hátralévő IDŐ"), "hatralevo ido");
        assert_eq!(fold("Größe"), "grosse");
        assert_eq!(fold("Sous-titres à côté"), "sous-titres a cote");
        assert_eq!(fold("Ёлка"), "елка");
    }

    #[test]
    fn every_word_must_appear_in_one_label() {
        let w = words("  speed DEFAULT ");
        assert_eq!(w, ["speed", "default"]);
        assert!(matches(&w, "Default speed", "Default speed"));
        assert!(!matches(&w, "Speed", "Speed"));
        // The translated label or the English one, but both words in the same label.
        let w = words("felirat");
        assert!(matches(&w, "Feliratok", "Subtitles"));
        let w = words("subtit");
        assert!(matches(&w, "Feliratok", "Subtitles"));
        let w = words("feliratok subtitles");
        assert!(!matches(&w, "Feliratok", "Subtitles"));
        // Accents typed or not.
        assert!(matches(&words("hatter"), "Fekete háttér (OLED)", "Black background (OLED)"));
        assert!(matches(&words("HÁTTÉR"), "Fekete hatter", "Black background"));
        assert!(!matches(&words("   "), "Anything", "Anything"));
    }

    #[test]
    fn the_query_and_start_row_are_taken_once() {
        submit("  hdr ");
        assert_eq!(take_query().as_deref(), Some("hdr"));
        assert_eq!(take_query(), None);
        set_start(SettingsPage::Markers, 3);
        assert_eq!(take_start(SettingsPage::Blocked), None);
        assert_eq!(take_start(SettingsPage::Markers), Some(3));
        assert_eq!(take_start(SettingsPage::Markers), None);
    }
}
