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

/// The interface language's code ("en" when English or untranslated).
pub fn current() -> &'static str {
    let p = CURRENT.load(Ordering::Relaxed);
    if p.is_null() {
        return "en";
    }
    tables().iter().find(|(_, m)| std::ptr::eq(*m as *const Map, p as *const Map)).map_or("en", |(k, _)| k)
}

/// A full date in words in the interface language (no abbreviations): `month` 1..=12.
pub fn full_date(year: i32, month: u32, day: u32) -> String {
    let m = (month.clamp(1, 12) - 1) as usize;
    match current() {
        "hu" => {
            const M: [&str; 12] = ["január", "február", "március", "április", "május", "június", "július", "augusztus", "szeptember", "október", "november", "december"];
            format!("{year}. {} {day}.", M[m])
        }
        "de" => {
            const M: [&str; 12] = ["Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember"];
            format!("{day}. {} {year}", M[m])
        }
        "es" => {
            const M: [&str; 12] = ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"];
            format!("{day} de {} de {year}", M[m])
        }
        "fr" => {
            const M: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];
            let d = if day == 1 { "1er".to_string() } else { day.to_string() };
            format!("{d} {} {year}", M[m])
        }
        "ru" => {
            const M: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
            format!("{day} {} {year}", M[m])
        }
        _ => {
            const M: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
            format!("{} {day}, {year}", M[m])
        }
    }
}

/// A date in words in the interface language: `weekday` 0 = Sunday, `month` 1..=12.
pub fn date_words(weekday: usize, day: u32, month: usize) -> String {
    let (w, m) = (weekday % 7, month.clamp(1, 12) - 1);
    match current() {
        "hu" => {
            const W: [&str; 7] = ["vasárnap", "hétfő", "kedd", "szerda", "csütörtök", "péntek", "szombat"];
            const M: [&str; 12] = ["január", "február", "március", "április", "május", "június", "július", "augusztus", "szeptember", "október", "november", "december"];
            format!("{} {day}., {}", M[m], W[w])
        }
        "de" => {
            const W: [&str; 7] = ["Sonntag", "Montag", "Dienstag", "Mittwoch", "Donnerstag", "Freitag", "Samstag"];
            const M: [&str; 12] = ["Januar", "Februar", "März", "April", "Mai", "Juni", "Juli", "August", "September", "Oktober", "November", "Dezember"];
            format!("{}, {day}. {}", W[w], M[m])
        }
        "es" => {
            const W: [&str; 7] = ["domingo", "lunes", "martes", "miércoles", "jueves", "viernes", "sábado"];
            const M: [&str; 12] = ["enero", "febrero", "marzo", "abril", "mayo", "junio", "julio", "agosto", "septiembre", "octubre", "noviembre", "diciembre"];
            format!("{}, {day} de {}", W[w], M[m])
        }
        "fr" => {
            const W: [&str; 7] = ["dimanche", "lundi", "mardi", "mercredi", "jeudi", "vendredi", "samedi"];
            const M: [&str; 12] = ["janvier", "février", "mars", "avril", "mai", "juin", "juillet", "août", "septembre", "octobre", "novembre", "décembre"];
            format!("{} {day} {}", W[w], M[m])
        }
        "ru" => {
            const W: [&str; 7] = ["воскресенье", "понедельник", "вторник", "среда", "четверг", "пятница", "суббота"];
            const M: [&str; 12] = ["января", "февраля", "марта", "апреля", "мая", "июня", "июля", "августа", "сентября", "октября", "ноября", "декабря"];
            format!("{}, {day} {}", W[w], M[m])
        }
        _ => {
            const W: [&str; 7] = ["Sunday", "Monday", "Tuesday", "Wednesday", "Thursday", "Friday", "Saturday"];
            const M: [&str; 12] = ["January", "February", "March", "April", "May", "June", "July", "August", "September", "October", "November", "December"];
            format!("{}, {} {day}", W[w], M[m])
        }
    }
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

/// Languages by code with their own names (what the app shows instead of codes), in the order
/// the subtitle language list offers them.
pub const LANGUAGES: &[(&str, &str)] = &[
    ("en", "English"),
    ("hu", "Magyar"),
    ("de", "Deutsch"),
    ("fr", "Français"),
    ("es", "Español"),
    ("it", "Italiano"),
    ("pt", "Português"),
    ("nl", "Nederlands"),
    ("pl", "Polski"),
    ("cs", "Čeština"),
    ("sk", "Slovenčina"),
    ("sl", "Slovenščina"),
    ("hr", "Hrvatski"),
    ("sr", "Српски"),
    ("bs", "Bosanski"),
    ("ro", "Română"),
    ("bg", "Български"),
    ("el", "Ελληνικά"),
    ("tr", "Türkçe"),
    ("ru", "Русский"),
    ("uk", "Українська"),
    ("be", "Беларуская"),
    ("lt", "Lietuvių"),
    ("lv", "Latviešu"),
    ("et", "Eesti"),
    ("fi", "Suomi"),
    ("sv", "Svenska"),
    ("no", "Norsk"),
    ("nb", "Norsk bokmål"),
    ("da", "Dansk"),
    ("is", "Íslenska"),
    ("ga", "Gaeilge"),
    ("ca", "Català"),
    ("eu", "Euskara"),
    ("gl", "Galego"),
    ("ar", "العربية"),
    ("he", "עברית"),
    ("iw", "עברית"),
    ("fa", "فارسی"),
    ("ur", "اردو"),
    ("hi", "हिन्दी"),
    ("bn", "বাংলা"),
    ("pa", "ਪੰਜਾਬੀ"),
    ("ta", "தமிழ்"),
    ("te", "తెలుగు"),
    ("mr", "मराठी"),
    ("gu", "ગુજરાતી"),
    ("kn", "ಕನ್ನಡ"),
    ("ml", "മലയാളം"),
    ("th", "ไทย"),
    ("vi", "Tiếng Việt"),
    ("id", "Bahasa Indonesia"),
    ("ms", "Bahasa Melayu"),
    ("fil", "Filipino"),
    ("tl", "Filipino"),
    ("ja", "日本語"),
    ("ko", "한국어"),
    ("zh-Hans", "中文（简体）"),
    ("zh-Hant", "中文（繁體）"),
    ("zh-CN", "中文（简体）"),
    ("zh-TW", "中文（繁體）"),
    ("zh", "中文"),
    ("sw", "Kiswahili"),
    ("af", "Afrikaans"),
    ("sq", "Shqip"),
    ("hy", "Հայերեն"),
    ("ka", "ქართული"),
    ("az", "Azərbaycan"),
    ("kk", "Қазақ"),
    ("uz", "Oʻzbek"),
    ("mn", "Монгол"),
    ("mk", "Македонски"),
    ("eo", "Esperanto"),
    ("la", "Latina"),
];

/// A language code as the language's own name ("hu" -> "Magyar", "pt-BR" -> "Português").
/// Unknown codes fall back to their base language, then to the code itself.
pub fn language_name(code: &str) -> String {
    if code == "system" {
        return tr("system").to_string();
    }
    let find = |c: &str| LANGUAGES.iter().find(|(k, _)| k.eq_ignore_ascii_case(c)).map(|(_, n)| n.to_string());
    find(code).or_else(|| code.split(['-', '_']).next().and_then(find)).unwrap_or_else(|| code.to_string())
}
