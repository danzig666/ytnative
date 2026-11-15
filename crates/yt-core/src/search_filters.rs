//! Search filters (SmartTube's set): upload date, type, duration, features and sort order,
//! encoded the way YouTube expects them in the search `params` (a small protobuf, base64).

use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum UploadDate {
    #[default]
    Any,
    Hour,
    Today,
    Week,
    Month,
    Year,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    #[default]
    Any,
    Video,
    Channel,
    Playlist,
    Movie,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Duration {
    #[default]
    Any,
    /// Under 4 minutes.
    Short,
    /// 4-20 minutes.
    Medium,
    /// Over 20 minutes.
    Long,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Sort {
    #[default]
    Relevance,
    Rating,
    UploadDate,
    ViewCount,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Filters {
    pub upload_date: UploadDate,
    pub kind: Kind,
    pub duration: Duration,
    pub live: bool,
    pub hd_4k: bool,
    pub hdr: bool,
    pub sort: Sort,
}

fn varint(out: &mut Vec<u8>, mut v: u64) {
    loop {
        let b = (v & 0x7f) as u8;
        v >>= 7;
        if v == 0 {
            out.push(b);
            return;
        }
        out.push(b | 0x80);
    }
}

fn field_varint(out: &mut Vec<u8>, field: u32, v: u64) {
    varint(out, ((field as u64) << 3) | 0);
    varint(out, v);
}

impl Filters {
    pub fn is_default(&self) -> bool {
        *self == Filters::default()
    }

    /// The `params` value for `search`, or None when no filter is set.
    pub fn params(&self) -> Option<String> {
        if self.is_default() {
            return None;
        }
        let mut inner = Vec::new();
        let up = match self.upload_date {
            UploadDate::Any => 0,
            UploadDate::Hour => 1,
            UploadDate::Today => 2,
            UploadDate::Week => 3,
            UploadDate::Month => 4,
            UploadDate::Year => 5,
        };
        if up != 0 {
            field_varint(&mut inner, 1, up);
        }
        let kind = match self.kind {
            Kind::Any => 0,
            Kind::Video => 1,
            Kind::Channel => 2,
            Kind::Playlist => 3,
            Kind::Movie => 4,
        };
        if kind != 0 {
            field_varint(&mut inner, 2, kind);
        }
        let dur = match self.duration {
            Duration::Any => 0,
            Duration::Short => 1,
            Duration::Long => 2,
            Duration::Medium => 3,
        };
        if dur != 0 {
            field_varint(&mut inner, 3, dur);
        }
        if self.live {
            field_varint(&mut inner, 8, 1);
        }
        if self.hd_4k {
            field_varint(&mut inner, 14, 1);
        }
        if self.hdr {
            field_varint(&mut inner, 25, 1);
        }
        let mut msg = Vec::new();
        let sort = match self.sort {
            // YouTube ignores the rating order, and since 2025 the upload date order too
            // (`sort_newest_first` orders the results on the TV; the value is still sent).
            Sort::Relevance | Sort::Rating => 0,
            Sort::UploadDate => 2,
            Sort::ViewCount => 3,
        };
        if sort != 0 {
            field_varint(&mut msg, 1, sort);
        }
        if !inner.is_empty() {
            varint(&mut msg, (2 << 3) | 2);
            varint(&mut msg, inner.len() as u64);
            msg.extend(inner);
        }
        Some(base64(&msg))
    }

    /// Short human summary for the results header ("This week · Videos · Over 20 min").
    pub fn summary(&self) -> String {
        let mut parts = Vec::new();
        match self.upload_date {
            UploadDate::Any => {}
            UploadDate::Hour => parts.push("Last hour"),
            UploadDate::Today => parts.push("Today"),
            UploadDate::Week => parts.push("This week"),
            UploadDate::Month => parts.push("This month"),
            UploadDate::Year => parts.push("This year"),
        }
        match self.kind {
            Kind::Any => {}
            Kind::Video => parts.push("Videos"),
            Kind::Channel => parts.push("Channels"),
            Kind::Playlist => parts.push("Playlists"),
            Kind::Movie => parts.push("Movies"),
        }
        match self.duration {
            Duration::Any => {}
            Duration::Short => parts.push("Under 4 min"),
            Duration::Medium => parts.push("4\u{2013}20 min"),
            Duration::Long => parts.push("Over 20 min"),
        }
        if self.live {
            parts.push("Live");
        }
        if self.hd_4k {
            parts.push("4K");
        }
        if self.hdr {
            parts.push("HDR");
        }
        match self.sort {
            Sort::Relevance => {}
            Sort::Rating => {}
            Sort::UploadDate => parts.push("newest first"),
            Sort::ViewCount => parts.push("most viewed"),
        }
        parts.join(" \u{00B7} ")
    }
}

/// Whether search `params` ask for the newest videos first (sort field 1 = 2).
pub fn newest_first(params: &str) -> bool {
    decode_base64(params).is_some_and(|b| b.starts_with(&[0x08, 0x02]))
}

/// How long ago a video was published, from its card's lines ("1.2K views • 3 days ago",
/// "Streamed 5 hours ago"), in seconds; live streams count as now. `None` for cards without an age
/// (channels, playlists). YouTube answers this app in English (`hl=en`).
pub fn age_secs(lines: &[String]) -> Option<u64> {
    for line in lines {
        if line.contains("watching") {
            return Some(0);
        }
        for part in line.split('\u{2022}') {
            let words: Vec<&str> = part.split_whitespace().collect();
            let Some(i) = words.iter().position(|w| *w == "ago") else { continue };
            if i < 2 {
                continue;
            }
            let Ok(n) = words[i - 2].parse::<u64>() else { continue };
            let unit = match words[i - 1].trim_end_matches('s') {
                "second" => 1,
                "minute" => 60,
                "hour" => 3600,
                "day" => 86_400,
                "week" => 7 * 86_400,
                "month" => 30 * 86_400,
                "year" => 365 * 86_400,
                _ => continue,
            };
            return Some(n * unit);
        }
    }
    None
}

/// Orders each shelf newest first, as YouTube no longer does for "sort by upload date": the cards
/// with an age are sorted among themselves in the slots they held; channels and playlists stay
/// where they were. Only within one page of results (each further page is sorted on its own).
pub fn sort_newest_first(page: &mut crate::model::Page) {
    for shelf in &mut page.shelves {
        let slots: Vec<usize> = (0..shelf.items.len()).filter(|i| age_secs(&shelf.items[*i].lines).is_some()).collect();
        let mut dated: Vec<crate::model::Item> = slots.iter().map(|i| shelf.items[*i].clone()).collect();
        dated.sort_by_key(|it| age_secs(&it.lines));
        for (slot, it) in slots.into_iter().zip(dated) {
            shelf.items[slot] = it;
        }
    }
}

fn decode_base64(s: &str) -> Option<Vec<u8>> {
    let val = |c: u8| -> Option<u32> {
        Some(match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' | b'-' => 62,
            b'/' | b'_' => 63,
            _ => return None,
        } as u32)
    };
    let s = s.replace("%3D", "=");
    let bytes: Vec<u8> = s.bytes().filter(|c| *c != b'=').collect();
    let mut out = Vec::new();
    for chunk in bytes.chunks(4) {
        let mut n = 0u32;
        for (k, c) in chunk.iter().enumerate() {
            n |= val(*c)? << (18 - 6 * k);
        }
        out.push((n >> 16) as u8);
        if chunk.len() > 2 {
            out.push((n >> 8) as u8);
        }
        if chunk.len() > 3 {
            out.push(n as u8);
        }
    }
    Some(out)
}

/// Standard base64 with padding (YouTube accepts it percent-encoded by the HTTP layer).
fn base64(data: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = String::new();
    for chunk in data.chunks(3) {
        let b = [chunk[0], *chunk.get(1).unwrap_or(&0), *chunk.get(2).unwrap_or(&0)];
        let n = ((b[0] as u32) << 16) | ((b[1] as u32) << 8) | b[2] as u32;
        out.push(T[(n >> 18) as usize & 63] as char);
        out.push(T[(n >> 12) as usize & 63] as char);
        out.push(if chunk.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        out.push(if chunk.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    // Reference strings are the well-known canonical values YouTube's own filter menu produces.
    #[test]
    fn single_filters_match_youtube() {
        let f = |f: Filters| f.params().unwrap();
        assert_eq!(f(Filters { upload_date: UploadDate::Hour, ..Default::default() }), "EgIIAQ==");
        assert_eq!(f(Filters { upload_date: UploadDate::Week, ..Default::default() }), "EgIIAw==");
        assert_eq!(f(Filters { kind: Kind::Video, ..Default::default() }), "EgIQAQ==");
        assert_eq!(f(Filters { kind: Kind::Channel, ..Default::default() }), "EgIQAg==");
        assert_eq!(f(Filters { duration: Duration::Short, ..Default::default() }), "EgIYAQ==");
        assert_eq!(f(Filters { duration: Duration::Long, ..Default::default() }), "EgIYAg==");
        assert_eq!(f(Filters { live: true, ..Default::default() }), "EgJAAQ==");
        assert_eq!(f(Filters { hd_4k: true, ..Default::default() }), "EgJwAQ==");
        assert_eq!(f(Filters { hdr: true, ..Default::default() }), "EgPIAQE=");
        assert_eq!(f(Filters { sort: Sort::UploadDate, ..Default::default() }), "CAI=");
        assert_eq!(f(Filters { sort: Sort::ViewCount, ..Default::default() }), "CAM=");
    }

    #[test]
    fn newest_first_is_read_back_from_the_params() {
        let p = |f: Filters| f.params().unwrap();
        assert!(newest_first(&p(Filters { sort: Sort::UploadDate, ..Default::default() })));
        assert!(newest_first(&p(Filters { sort: Sort::UploadDate, upload_date: UploadDate::Week, kind: Kind::Video, ..Default::default() })));
        assert!(!newest_first(&p(Filters { sort: Sort::ViewCount, ..Default::default() })));
        assert!(!newest_first(&p(Filters { upload_date: UploadDate::Today, ..Default::default() })));
        assert!(!newest_first("not base64!"));
    }

    #[test]
    fn the_rating_order_is_not_sent() {
        assert!(Filters { sort: Sort::Rating, ..Default::default() }.params().is_none_or(|p| !p.starts_with("CAE")));
    }

    #[test]
    fn ages_are_read_from_the_card_lines() {
        let l = |s: &str| vec![s.to_string()];
        assert_eq!(age_secs(&l("NASA \u{2022} 526K views \u{2022} 7 months ago")), Some(7 * 30 * 86_400));
        assert_eq!(age_secs(&["NASA".into(), "1M views \u{2022} 3 weeks ago".into()]), Some(21 * 86_400));
        assert_eq!(age_secs(&l("1 view \u{2022} 4 minutes ago")), Some(240));
        assert_eq!(age_secs(&l("3.5M views \u{2022} Streamed 1 hour ago")), Some(3600));
        assert_eq!(age_secs(&l("198 watching")), Some(0));
        assert_eq!(age_secs(&l("@NASA \u{2022} 15.1M subscribers")), None);
        assert_eq!(age_secs(&l("Drama \u{2022} 2018")), None);
    }

    #[test]
    fn newest_first_sorts_videos_and_leaves_channels_in_place() {
        use crate::model::{Item, Page, Shelf};
        let it = |id: &str, line: &str| Item { id: id.into(), lines: vec![line.into()], ..Default::default() };
        let mut page = Page {
            shelves: vec![Shelf {
                items: vec![it("old", "1 year ago"), it("chan", "@x \u{2022} 1M subscribers"), it("new", "2 hours ago"), it("mid", "3 days ago"), it("live", "12 watching")],
                ..Default::default()
            }],
            ..Default::default()
        };
        sort_newest_first(&mut page);
        let ids: Vec<&str> = page.shelves[0].items.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, ["live", "chan", "new", "mid", "old"]);
    }

    #[test]
    fn combined_filters() {
        let p = Filters { upload_date: UploadDate::Week, kind: Kind::Video, sort: Sort::UploadDate, ..Default::default() }.params().unwrap();
        // sort=2, filters{1:3, 2:1}
        assert_eq!(p, base64(&[0x08, 0x02, 0x12, 0x04, 0x08, 0x03, 0x10, 0x01]));
        assert!(Filters::default().params().is_none());
    }

    /// Diagnostic: what each filter returns from the live TV client.
    #[test]
    #[ignore]
    fn live_filter_survey() {
        let mut s = crate::session::Session::new().unwrap();
        let cases: Vec<(&str, Filters)> = vec![
            ("hour", Filters { upload_date: UploadDate::Hour, ..Default::default() }),
            ("month", Filters { upload_date: UploadDate::Month, ..Default::default() }),
            ("year", Filters { upload_date: UploadDate::Year, ..Default::default() }),
            ("medium 4-20", Filters { duration: Duration::Medium, ..Default::default() }),
            ("movie", Filters { kind: Kind::Movie, ..Default::default() }),
            ("hdr", Filters { hdr: true, ..Default::default() }),
            ("year+date", Filters { upload_date: UploadDate::Year, sort: Sort::UploadDate, ..Default::default() }),
        ];
        for (name, f) in cases {
            let page = s.search("nasa", f.params().as_deref()).unwrap();
            let items: Vec<_> = page.shelves.iter().flat_map(|s| s.items.iter()).take(6).collect();
            eprintln!("== {name} ({:?}): {} shelves", f.params(), page.shelves.len());
            for i in items {
                eprintln!("   {:?} {:?} | {} | {}", i.kind, i.badge, i.title.chars().take(40).collect::<String>(), i.lines.join(" / "));
            }
        }
    }

    #[test]
    #[ignore]
    fn live_filtered_search() {
        let mut s = crate::session::Session::new().unwrap();
        let p = Filters { kind: Kind::Channel, ..Default::default() }.params();
        let page = s.search("blender", p.as_deref()).unwrap();
        let kinds: Vec<_> = page.shelves.iter().flat_map(|s| s.items.iter()).map(|i| i.kind).collect();
        eprintln!("{kinds:?}");
        assert!(kinds.iter().any(|k| *k == crate::model::ItemKind::Channel));
    }
}

