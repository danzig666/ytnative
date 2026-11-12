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
            Sort::Relevance => 0,
            Sort::Rating => 1,
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
            Sort::Rating => parts.push("by rating"),
            Sort::UploadDate => parts.push("newest first"),
            Sort::ViewCount => parts.push("most viewed"),
        }
        parts.join(" \u{00B7} ")
    }
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
    fn combined_filters() {
        let p = Filters { upload_date: UploadDate::Week, kind: Kind::Video, sort: Sort::UploadDate, ..Default::default() }.params().unwrap();
        // sort=2, filters{1:3, 2:1}
        assert_eq!(p, base64(&[0x08, 0x02, 0x12, 0x04, 0x08, 0x03, 0x10, 0x01]));
        assert!(Filters::default().params().is_none());
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
