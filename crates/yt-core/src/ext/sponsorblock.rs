//! SponsorBlock (https://sponsor.ajay.app): crowd-sourced segments to skip (sponsors, intros, ...).
//!
//! Uses the privacy-preserving hash-prefix endpoint: only the first 4 hex chars of
//! sha256(video id) leave the device, and the matching video is picked from the answer locally.
//! Falls back to the alternate server when the main one fails.

use super::get_json;
use anyhow::Result;
use reqwest::blocking::Client;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::HashSet;

pub const SERVER: &str = "https://sponsor.ajay.app";
pub const SERVER_ALT: &str = "https://api.sponsor.ajay.app";

static ALT_FIRST: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Ask [`SERVER_ALT`] first (SmartTube's "alternative server"), falling back to [`SERVER`].
pub fn set_alt_server(first: bool) {
    ALT_FIRST.store(first, std::sync::atomic::Ordering::Relaxed);
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    Sponsor,
    Intro,
    Outro,
    Interaction,
    #[serde(rename = "selfpromo")]
    SelfPromo,
    #[serde(rename = "music_offtopic")]
    MusicOfftopic,
    /// Preview / recap.
    Preview,
    /// A single "highlight" point (action type `poi`).
    PoiHighlight,
    Filler,
    /// Whole-video label (action type `full`).
    ExclusiveAccess,
    /// Crowd chapter (action type `chapter`).
    Chapter,
    #[serde(other)]
    Unknown,
}

impl Category {
    /// The categories SmartTube offers, in its settings order.
    pub const ALL: [Category; 9] = [
        Category::Sponsor,
        Category::Intro,
        Category::Outro,
        Category::Interaction,
        Category::SelfPromo,
        Category::MusicOfftopic,
        Category::Preview,
        Category::PoiHighlight,
        Category::Filler,
    ];

    /// API name (`"sponsor"`, `"selfpromo"`, ...).
    pub fn as_str(&self) -> &'static str {
        match self {
            Category::Sponsor => "sponsor",
            Category::Intro => "intro",
            Category::Outro => "outro",
            Category::Interaction => "interaction",
            Category::SelfPromo => "selfpromo",
            Category::MusicOfftopic => "music_offtopic",
            Category::Preview => "preview",
            Category::PoiHighlight => "poi_highlight",
            Category::Filler => "filler",
            Category::ExclusiveAccess => "exclusive_access",
            Category::Chapter => "chapter",
            Category::Unknown => "unknown",
        }
    }

    /// English label for menus / toasts.
    pub fn label(&self) -> &'static str {
        match self {
            Category::Sponsor => "Sponsor",
            Category::Intro => "Intro",
            Category::Outro => "Outro / end cards",
            Category::Interaction => "Interaction reminder",
            Category::SelfPromo => "Self-promotion",
            Category::MusicOfftopic => "Non-music section",
            Category::Preview => "Preview / recap",
            Category::PoiHighlight => "Highlight",
            Category::Filler => "Filler tangent",
            Category::ExclusiveAccess => "Exclusive access",
            Category::Chapter => "Chapter",
            Category::Unknown => "Unknown",
        }
    }

    /// SmartTube's default action: skip with a toast for everything, except filler (off; it is
    /// "very extreme").
    pub fn default_action(&self) -> Action {
        match self {
            Category::Filler | Category::ExclusiveAccess | Category::Chapter | Category::Unknown => Action::None,
            _ => Action::SkipWithToast,
        }
    }
}

/// Seek-bar marker colour, `0xRRGGBB` (SmartTube's palette; SponsorBlock's for the extras).
pub fn colour(category: &Category) -> u32 {
    match category {
        Category::Sponsor => 0x00FF00,
        Category::Intro => 0x00FFFF,
        Category::Outro => 0x0000FF,
        Category::SelfPromo => 0xFFFF00,
        Category::Interaction => 0xFF00FF,
        Category::MusicOfftopic => 0xFF9900,
        Category::Preview => 0x2793E6,
        Category::PoiHighlight => 0xFF1684,
        Category::Filler => 0x7300FF,
        Category::ExclusiveAccess => 0x008A5C,
        Category::Chapter => 0xFFD679,
        Category::Unknown => 0x808080,
    }
}

/// What the player does when it reaches a segment of a category (SmartTube's choices).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Action {
    /// Do nothing (marker only, if enabled).
    None,
    /// Skip silently.
    Skip,
    /// Skip and show "Skipped: <category>".
    SkipWithToast,
    /// Ask the user whether to skip.
    Ask,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ActionType {
    Skip,
    Mute,
    /// Whole-video label; no time range.
    Full,
    /// Point of interest (start == end).
    Poi,
    Chapter,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Segment {
    pub category: Category,
    pub start_ms: u64,
    pub end_ms: u64,
    pub action_type: ActionType,
    pub uuid: String,
}

impl Segment {
    pub fn duration_ms(&self) -> u64 {
        self.end_ms.saturating_sub(self.start_ms)
    }
    /// Skip/mute segments are the ones that take effect while playing through them.
    pub fn is_ranged(&self) -> bool {
        matches!(self.action_type, ActionType::Skip | ActionType::Mute) && self.end_ms > self.start_ms
    }
}

#[derive(Deserialize)]
struct RawSegment {
    category: Category,
    #[serde(rename = "actionType", default = "default_action_type")]
    action_type: ActionType,
    segment: [f64; 2],
    #[serde(rename = "UUID", default)]
    uuid: String,
}

fn default_action_type() -> ActionType {
    ActionType::Skip
}

#[derive(Deserialize)]
struct HashedVideo {
    #[serde(rename = "videoID")]
    video_id: String,
    #[serde(default)]
    segments: Vec<RawSegment>,
}

fn secs_to_ms(s: f64) -> u64 {
    if s.is_finite() && s > 0.0 {
        (s * 1000.0).round() as u64
    } else {
        0
    }
}

fn convert(raw: Vec<RawSegment>) -> Vec<Segment> {
    let mut out: Vec<Segment> = raw
        .into_iter()
        .map(|r| {
            let (a, b) = (secs_to_ms(r.segment[0]), secs_to_ms(r.segment[1]));
            Segment { category: r.category, start_ms: a.min(b), end_ms: a.max(b), action_type: r.action_type, uuid: r.uuid }
        })
        .collect();
    out.sort_by_key(|s| (s.start_ms, s.end_ms));
    out
}

/// Parses the plain `/api/skipSegments?videoID=` answer (a segment array).
pub fn parse_segments(json: &str) -> Result<Vec<Segment>> {
    Ok(convert(serde_json::from_str(json)?))
}

/// Parses the hash-prefix answer and keeps the segments of `video_id`.
pub fn parse_hashed(json: &str, video_id: &str) -> Result<Vec<Segment>> {
    let videos: Vec<HashedVideo> = serde_json::from_str(json)?;
    Ok(videos.into_iter().find(|v| v.video_id == video_id).map(|v| convert(v.segments)).unwrap_or_default())
}

/// First 4 hex chars of sha256(video id), as the hash-prefix endpoint wants.
pub fn hash_prefix(video_id: &str) -> String {
    let digest = Sha256::digest(video_id.as_bytes());
    format!("{:02x}{:02x}", digest[0], digest[1])
}

fn json_list<'a>(items: impl Iterator<Item = &'a str>) -> String {
    let v: Vec<&str> = items.collect();
    serde_json::to_string(&v).unwrap()
}

/// Segments of `video_id` in `categories` (all action types), sorted by start. Empty when the
/// video has none. Tries [`SERVER`], then [`SERVER_ALT`].
pub fn fetch(http: &Client, video_id: &str, categories: &[Category]) -> Result<Vec<Segment>> {
    let cats = json_list(categories.iter().map(|c| c.as_str()));
    let actions = json_list(["skip", "mute", "full", "poi", "chapter"].into_iter());
    let path = format!("/api/skipSegments/{}", hash_prefix(video_id));
    let query = [("categories", cats.as_str()), ("actionTypes", actions.as_str())];
    let mut last_err = None;
    let servers = if ALT_FIRST.load(std::sync::atomic::Ordering::Relaxed) { [SERVER_ALT, SERVER] } else { [SERVER, SERVER_ALT] };
    for server in servers {
        match get_json::<Vec<HashedVideo>>(http, &format!("{server}{path}"), &query) {
            Ok(None) => return Ok(Vec::new()),
            Ok(Some(videos)) => {
                return Ok(videos.into_iter().find(|v| v.video_id == video_id).map(|v| convert(v.segments)).unwrap_or_default())
            }
            Err(e) => last_err = Some(e),
        }
    }
    Err(last_err.unwrap())
}

/// The ranged (skip/mute) segment playing at `pos_ms`, if any. When segments overlap, the one
/// that ends last wins (skipping it clears the others too).
pub fn segment_at(segments: &[Segment], pos_ms: u64) -> Option<&Segment> {
    segments
        .iter()
        .filter(|s| s.is_ranged() && s.start_ms <= pos_ms && pos_ms < s.end_ms)
        .max_by_key(|s| s.end_ms)
}

/// "Act on each segment once" bookkeeping for the player's position ticks.
///
/// Call [`SkipTracker::check`] on every tick; it returns a segment only on *entering* it, so a
/// mute/ask segment does not re-trigger every tick. With `skip_again == false` (SmartTube's
/// "don't skip segment again"), a segment already acted on is ignored when the user seeks back.
#[derive(Debug, Default)]
pub struct SkipTracker {
    pub skip_again: bool,
    /// Segments shorter than this are ignored (SmartTube's "ignore short segments").
    pub min_duration_ms: u64,
    done: HashSet<String>,
    inside: Option<String>,
}

impl SkipTracker {
    pub fn new(skip_again: bool) -> Self {
        SkipTracker { skip_again, ..Default::default() }
    }

    /// Forget everything (new video).
    pub fn reset(&mut self) {
        self.done.clear();
        self.inside = None;
    }

    /// The segment to act on at `pos_ms`, at most once per entry.
    pub fn check<'a>(&mut self, segments: &'a [Segment], pos_ms: u64) -> Option<&'a Segment> {
        self.check_where(segments, pos_ms, |_| true)
    }

    /// As [`check`](Self::check), among the segments `wanted` accepts only (the categories the
    /// viewer acts on): a longer overlapping segment of a category that is off must not hide a
    /// sponsor inside it.
    pub fn check_where<'a>(&mut self, segments: &'a [Segment], pos_ms: u64, wanted: impl Fn(&Segment) -> bool) -> Option<&'a Segment> {
        let min = self.min_duration_ms;
        let seg = segments
            .iter()
            .filter(|s| s.is_ranged() && s.duration_ms() >= min && s.start_ms <= pos_ms && pos_ms < s.end_ms && wanted(s))
            .max_by_key(|s| s.end_ms);
        let Some(seg) = seg else {
            self.inside = None;
            return None;
        };
        if self.inside.as_deref() == Some(seg.uuid.as_str()) {
            return None;
        }
        self.inside = Some(seg.uuid.clone());
        let first_time = self.done.insert(seg.uuid.clone());
        if first_time || self.skip_again {
            Some(seg)
        } else {
            None
        }
    }

    /// Mark a segment as handled without acting (e.g. the user declined the "skip?" prompt).
    pub fn mark_done(&mut self, seg: &Segment) {
        self.done.insert(seg.uuid.clone());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PLAIN: &str = r#"[
      {"category":"sponsor","actionType":"skip","segment":[30.5,60.25],"UUID":"u2","locked":0,"votes":3,"videoDuration":212.0},
      {"category":"intro","actionType":"skip","segment":[0,5],"UUID":"u1","votes":1},
      {"category":"poi_highlight","actionType":"poi","segment":[100,100],"UUID":"u3"},
      {"category":"selfpromo","actionType":"mute","segment":[150,160],"UUID":"u4"},
      {"category":"brand_new_thing","actionType":"skip","segment":[170,171],"UUID":"u5"},
      {"category":"exclusive_access","actionType":"full","segment":[0,0],"UUID":"u6"}
    ]"#;

    #[test]
    fn parses_plain() {
        let s = parse_segments(PLAIN).unwrap();
        assert_eq!(s.len(), 6);
        assert_eq!(s[1].uuid, "u1");
        assert_eq!(s[2].category, Category::Sponsor);
        assert_eq!((s[2].start_ms, s[2].end_ms), (30_500, 60_250));
        let poi = s.iter().find(|x| x.uuid == "u3").unwrap();
        assert_eq!((poi.category.clone(), poi.action_type), (Category::PoiHighlight, ActionType::Poi));
        assert_eq!(s.iter().find(|x| x.uuid == "u5").unwrap().category, Category::Unknown);
        assert_eq!(s.iter().find(|x| x.uuid == "u6").unwrap().action_type, ActionType::Full);
    }

    #[test]
    fn parses_hashed() {
        let json = format!(r#"[{{"videoID":"other","hash":"x","segments":[]}},{{"videoID":"abc","hash":"y","segments":{PLAIN}}}]"#);
        assert_eq!(parse_hashed(&json, "abc").unwrap().len(), 6);
        assert!(parse_hashed(&json, "nope").unwrap().is_empty());
    }

    #[test]
    fn hash_prefix_matches_sha256() {
        // sha256("abc") = ba7816bf...
        assert_eq!(hash_prefix("abc"), "ba78");
    }

    #[test]
    fn lookup_and_tracker() {
        let s = parse_segments(PLAIN).unwrap();
        assert_eq!(segment_at(&s, 2_000).unwrap().uuid, "u1");
        assert_eq!(segment_at(&s, 5_000), None); // end is exclusive
        assert_eq!(segment_at(&s, 100_000), None); // poi is not ranged
        assert_eq!(segment_at(&s, 0).unwrap().uuid, "u1"); // full label ignored
        assert_eq!(segment_at(&s, 155_000).unwrap().action_type, ActionType::Mute);

        let mut t = SkipTracker::new(false);
        assert_eq!(t.check(&s, 1_000).unwrap().uuid, "u1");
        assert!(t.check(&s, 1_500).is_none()); // still inside: once per entry
        assert!(t.check(&s, 10_000).is_none());
        assert!(t.check(&s, 1_000).is_none()); // seeked back: already done
        let mut again = SkipTracker::new(true);
        again.check(&s, 1_000);
        again.check(&s, 10_000);
        assert!(again.check(&s, 1_000).is_some());
        let mut short = SkipTracker { min_duration_ms: 10_000, ..Default::default() };
        assert!(short.check(&s, 1_000).is_none());
        t.reset();
        assert!(t.check(&s, 1_000).is_some());
    }

    #[test]
    fn an_ignored_category_does_not_hide_an_overlapping_one() {
        let s = parse_segments(r#"[
          {"category":"sponsor","actionType":"skip","segment":[30,60],"UUID":"s"},
          {"category":"filler","actionType":"skip","segment":[20,90],"UUID":"f"}]"#).unwrap();
        let mut t = SkipTracker::new(false);
        let seg = t.check_where(&s, 40_000, |x| x.category != Category::Filler).unwrap();
        assert_eq!(seg.uuid, "s");
    }

    #[test]
    fn defaults_and_colours() {
        assert_eq!(Category::Filler.default_action(), Action::None);
        assert_eq!(Category::Sponsor.default_action(), Action::SkipWithToast);
        assert_eq!(colour(&Category::Sponsor), 0x00FF00);
        assert_eq!(Category::MusicOfftopic.as_str(), "music_offtopic");
    }

    #[test]
    #[ignore = "hits sponsor.ajay.app"]
    fn live_fetch() {
        let http = Client::new();
        // A video with sponsor/intro/outro/selfpromo segments (picked from the "aaaa" hash bucket).
        let id = "WXGV2sWjN4E";
        assert_eq!(hash_prefix(id), "aaaa");
        let segs = fetch(&http, id, &Category::ALL).unwrap();
        println!("{segs:#?}");
        assert!(segs.iter().any(|s| s.category == Category::Sponsor && s.is_ranged()));
        assert!(fetch(&http, "dQw4w9WgXcQ", &[Category::Sponsor]).unwrap().is_empty());
    }
}
