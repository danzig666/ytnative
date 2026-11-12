//! DeArrow (https://dearrow.ajay.app): crowd-sourced, non-clickbait titles and thumbnails.

use super::get_json;
use anyhow::Result;
use reqwest::blocking::Client;
use serde::Deserialize;

pub const BRANDING_URL: &str = "https://sponsor.ajay.app/api/branding";
pub const THUMBNAIL_URL: &str = "https://dearrow-thumb.ajay.app/api/v1/getThumbnail";

#[derive(Debug, Clone, Default, Deserialize)]
pub struct Branding {
    #[serde(default)]
    pub titles: Vec<TitleSubmission>,
    #[serde(default)]
    pub thumbnails: Vec<ThumbSubmission>,
    /// Fraction (0..1) of the video DeArrow suggests as a random frame when nobody submitted one.
    #[serde(rename = "randomTime")]
    pub random_time: Option<f64>,
    #[serde(rename = "videoDuration")]
    pub video_duration: Option<f64>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct TitleSubmission {
    pub title: String,
    #[serde(default)]
    pub original: bool,
    #[serde(default)]
    pub votes: i64,
    #[serde(default)]
    pub locked: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct ThumbSubmission {
    /// Seconds into the video; absent for an "original thumbnail" vote.
    pub timestamp: Option<f64>,
    #[serde(default)]
    pub original: bool,
    #[serde(default)]
    pub votes: i64,
    #[serde(default)]
    pub locked: bool,
}

/// DeArrow's trust rule: a submission counts if it is locked or not voted below zero.
fn trusted(votes: i64, locked: bool) -> bool {
    locked || votes >= 0
}

impl Branding {
    /// The best crowd title, ready to show; `None` when the original title should stay.
    /// Submissions arrive best-first; the first trusted one wins, and if that is the original
    /// title, nothing is replaced.
    pub fn title(&self) -> Option<String> {
        let best = self.titles.iter().find(|t| trusted(t.votes, t.locked))?;
        if best.original {
            return None;
        }
        let t = strip_format_markers(&best.title);
        (!t.trim().is_empty()).then_some(t)
    }

    /// Timestamp (seconds) of the best crowd thumbnail; `None` to keep the original.
    pub fn thumbnail_time(&self) -> Option<f64> {
        let best = self.thumbnails.iter().find(|t| trusted(t.votes, t.locked))?;
        if best.original {
            return None;
        }
        best.timestamp
    }

    /// DeArrow's random-frame fallback (seconds), when no thumbnail was submitted. The server
    /// often omits `videoDuration`; pass the duration you know from the player/browse data.
    pub fn random_frame_time(&self, duration_secs: Option<f64>) -> Option<f64> {
        if !self.thumbnails.is_empty() {
            return None;
        }
        let duration = self.video_duration.or(duration_secs).filter(|d| *d > 0.0)?;
        Some(self.random_time? * duration)
    }

    /// Crowd thumbnail URL for `video_id`, if one should replace the original.
    pub fn thumbnail_url(&self, video_id: &str) -> Option<String> {
        self.thumbnail_time().map(|t| thumbnail_url(video_id, t))
    }
}

/// DeArrow titles use `>` before a word to say "leave this word's casing alone"; drop the markers
/// (a `>` that starts a word), keep any other `>` (e.g. "A > B").
pub fn strip_format_markers(title: &str) -> String {
    let chars: Vec<char> = title.chars().collect();
    let mut out = String::with_capacity(title.len());
    for (i, &c) in chars.iter().enumerate() {
        let at_word_start = i == 0 || chars[i - 1].is_whitespace();
        let before_word = chars.get(i + 1).is_some_and(|n| !n.is_whitespace());
        if c == '>' && at_word_start && before_word {
            continue;
        }
        out.push(c);
    }
    out
}

/// Thumbnail-render URL for a frame at `time_secs`.
pub fn thumbnail_url(video_id: &str, time_secs: f64) -> String {
    format!("{THUMBNAIL_URL}?videoID={video_id}&time={time_secs}")
}

/// Branding for `video_id`; `Ok(None)` when DeArrow has nothing.
pub fn fetch(http: &Client, video_id: &str) -> Result<Option<Branding>> {
    get_json(http, BRANDING_URL, &[("videoID", video_id)])
}

/// SmartTube's "thumbnail quality" option for videos without a DeArrow thumbnail: YouTube's
/// auto-generated frames instead of the uploader's (often clickbait) image.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum FallbackFrame {
    /// Keep the uploader's thumbnail.
    #[default]
    Default,
    Start,
    Middle,
    End,
}

impl FallbackFrame {
    fn file(self) -> &'static str {
        match self {
            FallbackFrame::Default => "hqdefault",
            FallbackFrame::Start => "hq1",
            FallbackFrame::Middle => "hq2",
            FallbackFrame::End => "hq3",
        }
    }

    /// `https://i.ytimg.com/vi/<id>/<frame>.jpg`.
    pub fn url(self, video_id: &str) -> String {
        format!("https://i.ytimg.com/vi/{video_id}/{}.jpg", self.file())
    }

    /// Rewrites a ytimg thumbnail URL (`.../hqdefault.jpg`, `mqdefault`, `sddefault`, `hq720`,
    /// `hq1..3`) to this frame, as SmartTube does. Other URLs and `Default` are returned unchanged.
    pub fn rewrite(self, thumb_url: &str) -> String {
        if self == FallbackFrame::Default {
            return thumb_url.to_string();
        }
        for q in ["hqdefault", "mqdefault", "sddefault", "hq720", "hq1", "hq2", "hq3"] {
            let pat = format!("/{q}.");
            if let Some(i) = thumb_url.find(&pat) {
                return format!("{}/{}.{}", &thumb_url[..i], self.file(), &thumb_url[i + pat.len()..]);
            }
        }
        thumb_url.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(json: &str) -> Branding {
        serde_json::from_str(json).unwrap()
    }

    #[test]
    fn picks_trusted_title_and_strips_markers() {
        let b = parse(
            r#"{"titles":[
                {"title":"Bad","original":false,"votes":-2,"locked":false,"UUID":"a"},
                {"title":">iPhone review: >USB-C is fine > Lightning","original":false,"votes":0,"locked":false,"UUID":"b"},
                {"title":"Original","original":true,"votes":5,"locked":false,"UUID":"c"}],
              "thumbnails":[{"timestamp":42.5,"original":false,"votes":1,"locked":false,"UUID":"t"}],
              "randomTime":0.5,"videoDuration":100}"#,
        );
        assert_eq!(b.title().as_deref(), Some("iPhone review: USB-C is fine > Lightning"));
        assert_eq!(b.thumbnail_time(), Some(42.5));
        assert_eq!(b.random_frame_time(None), None);
        assert_eq!(
            b.thumbnail_url("vid").as_deref(),
            Some("https://dearrow-thumb.ajay.app/api/v1/getThumbnail?videoID=vid&time=42.5")
        );
    }

    #[test]
    fn locked_beats_votes_and_original_keeps_title() {
        let b = parse(r#"{"titles":[{"title":"Locked","votes":-5,"locked":true}],"thumbnails":[]}"#);
        assert_eq!(b.title().as_deref(), Some("Locked"));
        let b = parse(
            r#"{"titles":[{"title":"Orig","original":true,"votes":3},{"title":"Alt","votes":1}],
                "thumbnails":[{"timestamp":null,"original":true,"votes":2}],"randomTime":0.25,"videoDuration":200}"#,
        );
        assert_eq!(b.title(), None);
        assert_eq!(b.thumbnail_time(), None);
        let empty = parse(r#"{"titles":[],"thumbnails":[],"randomTime":0.25,"videoDuration":200}"#);
        assert_eq!(empty.random_frame_time(None), Some(50.0));
        let no_dur = parse(r#"{"titles":[],"thumbnails":[],"randomTime":0.5}"#);
        assert_eq!(no_dur.random_frame_time(None), None);
        assert_eq!(no_dur.random_frame_time(Some(10.0)), Some(5.0));
    }

    #[test]
    fn fallback_frames() {
        assert_eq!(FallbackFrame::Middle.url("x"), "https://i.ytimg.com/vi/x/hq2.jpg");
        let u = "https://i.ytimg.com/vi/x/hqdefault.jpg?sqp=abc";
        assert_eq!(FallbackFrame::End.rewrite(u), "https://i.ytimg.com/vi/x/hq3.jpg?sqp=abc");
        assert_eq!(FallbackFrame::Default.rewrite(u), u);
        assert_eq!(FallbackFrame::Start.rewrite("https://example.com/a.jpg"), "https://example.com/a.jpg");
    }

    #[test]
    #[ignore = "hits sponsor.ajay.app"]
    fn live_fetch() {
        let http = Client::new();
        let b = fetch(&http, "dQw4w9WgXcQ").unwrap().expect("branding");
        println!("title={:?} thumb={:?} raw={b:#?}", b.title(), b.thumbnail_url("dQw4w9WgXcQ"));
        assert!(!b.titles.is_empty() || b.random_time.is_some());
    }
}
