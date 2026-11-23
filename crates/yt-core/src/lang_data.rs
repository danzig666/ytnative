//! YouTube's wording in every content language ytnative asks it in (`innertube::set_content_language`):
//! the ages under the cards ("3 days ago", "قبل 3 أيام", "3天前"), live viewers, past streams,
//! month names and the titles of the Watch later and Liked playlists. The tables are generated
//! (`tools/gen-lang-data.py`) from CLDR, whose relative-time wording YouTube uses, and from
//! YouTube's own answers for the rest.

#[path = "lang_data_gen.rs"]
mod generated;

/// One language's wording, lower case and [`norm`]alized.
pub struct Lang {
    pub hl: &'static str,
    /// Each month's names: wide and abbreviated, in date and stand-alone forms.
    pub months: [&'static [&'static str]; 12],
    pub ago: &'static [Ago],
    pub watching: &'static [&'static str],
    pub streamed: &'static [&'static str],
    pub later: &'static str,
    pub liked: &'static str,
}

/// One way to say "N units ago": `before` N `after` ("il y a " N " jours"), or, with `n` set, a
/// fixed phrase for that count in `before` ("قبل يومين", two days).
pub struct Ago {
    pub before: &'static str,
    pub after: &'static str,
    pub n: u64,
    pub secs: u64,
}

/// Lower case, every kind of space as one plain space, no bidi marks: how the tables are written.
pub fn norm(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut space = false;
    for c in s.chars() {
        if matches!(c, '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}') {
            continue;
        }
        if c.is_whitespace() {
            space = true;
            continue;
        }
        if space && !out.is_empty() {
            out.push(' ');
        }
        space = false;
        out.extend(c.to_lowercase());
    }
    out
}

/// The table for a YouTube `hl` ("de", "zh-TW"), or for its base language ("pt-PT" -> "pt").
pub fn get(hl: &str) -> Option<&'static Lang> {
    let find = |h: &str| generated::LANGS.iter().find(|l| l.hl.eq_ignore_ascii_case(h));
    find(hl).or_else(|| find(hl.split('-').next().unwrap_or(hl)))
}

/// The table of the language YouTube is asked in (English when none is set).
pub fn current() -> &'static Lang {
    crate::innertube::content_language().and_then(|hl| get(&hl)).or_else(|| get("en")).expect("English table")
}

/// Every language's table.
pub fn all() -> &'static [Lang] {
    generated::LANGS
}

impl Lang {
    /// How long ago `part` (one "•"-separated piece of a card line, any case) says, in seconds.
    pub fn age(&self, part: &str) -> Option<u64> {
        let p = format!(" {} ", norm(part));
        let mut best: Option<(usize, u64)> = None;
        let mut keep = |len: usize, secs: u64| {
            if best.is_none_or(|(l, _)| len > l) {
                best = Some((len, secs));
            }
        };
        for a in self.ago {
            if a.n > 0 {
                if contains_phrase(&p, a.before) {
                    keep(a.before.len(), a.n * a.secs);
                }
                continue;
            }
            for (start, end) in digit_runs(&p) {
                if !(p[..start].ends_with(a.before) && p[end..].starts_with(a.after)) {
                    continue;
                }
                let (pre, post) = (&p[..start - a.before.len()], &p[end + a.after.len()..]);
                if word_edge(a.before.chars().next(), pre.chars().next_back()) && word_edge(a.after.chars().next_back(), post.chars().next()) {
                    if let Ok(n) = p[start..end].parse::<u64>() {
                        keep(a.before.len() + a.after.len(), n * a.secs);
                    }
                }
            }
        }
        best.map(|(_, s)| s)
    }

    /// A live stream's viewer line ("9898 人が視聴中").
    pub fn is_watching(&self, line: &str) -> bool {
        let l = norm(line);
        self.watching.iter().any(|w| l.contains(w))
    }

    /// A past live stream's age ("Streamed 2 days ago", "2 yıl önce yayınlandı"): the marker
    /// in the same piece of the line as an age, so a channel named "… streaming" is not one.
    pub fn is_streamed(&self, line: &str) -> bool {
        line.split('\u{2022}').any(|part| {
            let l = norm(part);
            self.streamed.iter().any(|w| l.contains(w)) && self.age(part).is_some()
        })
    }

    /// The month (1..=12) a word of a date names ("apr", "kwietnia", "באפר׳", "d’abr").
    pub fn month(&self, word: &str) -> Option<u32> {
        let w = norm(word);
        let w = w.trim_end_matches('.');
        let mut cands = vec![w];
        for pre in ["d’", "d'", "ב"] {
            if let Some(rest) = w.strip_prefix(pre) {
                cands.push(rest);
            }
        }
        let mut best: Option<(usize, u32)> = None;
        for c in cands {
            for (i, names) in self.months.iter().enumerate() {
                for n in names.iter() {
                    let ok = c == *n || (n.chars().count() >= 3 && c.starts_with(n));
                    if ok && best.is_none_or(|(l, _)| n.len() > l) {
                        best = Some((n.len(), i as u32 + 1));
                    }
                }
            }
        }
        best.map(|(_, m)| m)
    }
}

/// `phrase` in `p` (both padded with spaces) as whole words where the script has spaces.
fn contains_phrase(p: &str, phrase: &str) -> bool {
    !phrase.is_empty() && p.contains(phrase)
}

/// A form's letter at its edge is not glued to more letters ("2 m" is not "2 mln"), in the
/// scripts that separate words with spaces (below Hebrew; CJK and Thai run words together).
fn word_edge(form: Option<char>, next: Option<char>) -> bool {
    let wordy = |c: char| c.is_alphanumeric() && (c as u32) < 0x0590;
    !(form.is_some_and(wordy) && next.is_some_and(wordy))
}

/// The byte ranges of the ASCII digit runs in `s`.
fn digit_runs(s: &str) -> Vec<(usize, usize)> {
    let b = s.as_bytes();
    let mut out = Vec::new();
    let mut i = 0;
    while i < b.len() {
        if b[i].is_ascii_digit() {
            let start = i;
            while i < b.len() && b[i].is_ascii_digit() {
                i += 1;
            }
            out.push((start, i));
        } else {
            i += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn age(hl: &str, s: &str) -> Option<u64> {
        get(hl).unwrap().age(s)
    }

    const D: u64 = 86_400;

    #[test]
    fn every_language_has_a_table() {
        for hl in ["en", "hu", "pl", "pt", "iw", "no", "zh-CN", "zh-TW", "ja", "ar", "th"] {
            assert!(get(hl).is_some(), "{hl}");
        }
        assert_eq!(get("pt-PT").unwrap().hl, "pt");
    }

    /// Card lines as YouTube wrote them (ytcli with YTCLI_HL, November 2026).
    #[test]
    fn ages_as_youtube_writes_them() {
        assert_eq!(age("pl", "15 lat temu"), Some(15 * 365 * D));
        assert_eq!(age("pl", "7 miesięcy temu"), Some(7 * 30 * D));
        assert_eq!(age("tr", "19 saat önce"), Some(19 * 3600));
        assert_eq!(age("az", "4 gün əvvəl"), Some(4 * D));
        assert_eq!(age("az", "Yayım vaxtı: 2 il əvvəl"), Some(2 * 365 * D));
        assert_eq!(age("ar", "قبل 5 ساعات"), Some(5 * 3600));
        assert_eq!(age("iw", "לפני 21 שנים"), Some(21 * 365 * D));
        assert_eq!(age("ja", "4 日前"), Some(4 * D));
        assert_eq!(age("zh-CN", "2个月前"), Some(60 * D));
        assert_eq!(age("zh-CN", "5小时前"), Some(5 * 3600));
        assert_eq!(age("zh-TW", "10 天前"), Some(10 * D));
        assert_eq!(age("ko", "13일 전"), Some(13 * D));
        assert_eq!(age("th", "2 วันที่ผ่านมา"), Some(2 * D));
        assert_eq!(age("vi", "1 tháng trước"), Some(30 * D));
        assert_eq!(age("hi", "21 वर्ष पहले"), Some(21 * 365 * D));
        assert_eq!(age("te", "2 రోజుల క్రితం"), Some(2 * D));
        assert_eq!(age("fa", "3 سال پیش"), Some(3 * 365 * D));
        assert_eq!(age("no", "for 3 døgn siden"), Some(3 * D));
        assert_eq!(age("sv", "för 2 timmar sedan"), Some(2 * 3600));
        assert_eq!(age("lv", "pirms 1 mēneša"), Some(30 * D));
        assert_eq!(age("sk", "pred 7 dňami"), Some(7 * D));
        assert_eq!(age("ro", "acum 21 de ani"), Some(21 * 365 * D));
        assert_eq!(age("uk", "1 годину тому"), Some(3600));
        assert_eq!(age("id", "1 tahun yang lalu"), Some(365 * D));
        // Not ages: views, subscribers, live viewers.
        assert_eq!(age("pl", "1,1 mln wyświetleń"), None);
        assert_eq!(age("ja", "35万回視聴"), None);
        assert_eq!(age("zh-CN", "1580万位订阅者"), None);
        assert_eq!(age("ko", "조회수 1.5만회"), None);
    }

    #[test]
    fn live_viewers_and_past_streams() {
        assert!(get("ja").unwrap().is_watching("9898 人が視聴中"));
        assert!(!get("ja").unwrap().is_watching("35万回視聴"));
        assert!(get("vi").unwrap().is_watching("812 người đang xem"));
        assert!(!get("vi").unwrap().is_watching("1,2 N lượt xem"));
        assert!(get("tr").unwrap().is_streamed("1,5 Mn görüntüleme • 1 yıl önce yayınlandı"));
        assert!(!get("tr").unwrap().is_streamed("1,5 Mn görüntüleme • 1 yıl önce"));
        assert!(get("id").unwrap().is_streamed("Streaming 2 hari yang lalu"));
        assert!(!get("id").unwrap().is_streamed("Lofi streaming • 2 rb x ditonton"));
        assert!(get("zh-CN").unwrap().is_streamed("直播时间：3年前"));
    }

    #[test]
    fn month_names() {
        assert_eq!(get("pl").unwrap().month("kwi"), Some(4));
        assert_eq!(get("pl").unwrap().month("listopada"), Some(11));
        assert_eq!(get("hr").unwrap().month("lis"), Some(10), "listopad is October in Croatian");
        assert_eq!(get("iw").unwrap().month("באפר׳"), Some(4));
        assert_eq!(get("ca").unwrap().month("d’abr."), Some(4));
        assert_eq!(get("th").unwrap().month("เม.ย."), Some(4));
        assert_eq!(get("el").unwrap().month("Απρ"), Some(4));
        assert_eq!(get("hy").unwrap().month("ապր"), Some(4));
        assert_eq!(get("fr").unwrap().month("juil."), Some(7));
        assert_eq!(get("fr").unwrap().month("juin"), Some(6));
    }
}
