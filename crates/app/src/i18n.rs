//! Interface translations. `i18n/strings.tsv` holds one row per English string and one column per
//! language; [`ui::text`](crate::ui::text) looks every drawn string up, so screens keep writing
//! English literals. Strings missing from the table (video titles, formatted values) pass through.

use std::collections::HashMap;
use std::sync::atomic::{AtomicPtr, Ordering};
use std::sync::OnceLock;

const TABLE: &str = include_str!("../i18n/strings.tsv");

type Map = HashMap<&'static str, &'static str>;

/// Languages with a column in the table (besides English).
pub fn languages() -> Vec<&'static str> {
    TABLE.lines().next().unwrap_or("").split('\t').skip(1).collect()
}

fn tables() -> &'static HashMap<&'static str, Map> {
    static T: OnceLock<HashMap<&'static str, Map>> = OnceLock::new();
    T.get_or_init(|| {
        let mut lines = TABLE.lines();
        let head: Vec<&str> = lines.next().unwrap_or("").split('\t').collect();
        let mut out: HashMap<&'static str, Map> = head.iter().skip(1).map(|l| (*l, Map::new())).collect();
        for line in lines.filter(|l| !l.trim().is_empty()) {
            let cols: Vec<&'static str> = line.split('\t').collect();
            let Some(en) = cols.first() else { continue };
            for (i, lang) in head.iter().enumerate().skip(1) {
                if let Some(t) = cols.get(i).filter(|t| !t.is_empty()) {
                    out.get_mut(lang).unwrap().insert(*en, *t);
                }
            }
        }
        out
    })
}

static CURRENT: AtomicPtr<Map> = AtomicPtr::new(std::ptr::null_mut());

/// `pref` is the setting ("system", "en", "hu", ...); `system` is the TV's locale (e.g. "hu-HU").
pub fn set_language(pref: &str, system: &str) {
    let code = if pref == "system" { system.split(['-', '_']).next().unwrap_or("en") } else { pref };
    let map = tables().get(code).map_or(std::ptr::null_mut(), |m| m as *const Map as *mut Map);
    CURRENT.store(map, Ordering::Relaxed);
}

/// The translation of `s`, or `s` itself.
pub fn tr(s: &str) -> &str {
    let p = CURRENT.load(Ordering::Relaxed);
    if p.is_null() {
        return s;
    }
    // SAFETY: points into the 'static `tables()` map, never freed.
    let map: &'static Map = unsafe { &*p };
    map.get(s).copied().unwrap_or(s)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_is_rectangular_and_translates() {
        let width = TABLE.lines().next().unwrap().split('\t').count();
        for (n, l) in TABLE.lines().enumerate().filter(|(_, l)| !l.is_empty()) {
            assert_eq!(l.split('\t').count(), width, "line {}: {l}", n + 1);
        }
        assert!(languages().contains(&"hu"));
        set_language("system", "de-DE");
        assert_eq!(tr("Settings"), "Einstellungen");
        assert_eq!(tr("Some video title"), "Some video title");
        set_language("en", "");
        assert_eq!(tr("Settings"), "Settings");
    }
}
