//! Format choice for the Feed engine.
//!
//! Rules (webOS findings, see `docs/feed-engine.md`): YouTube's > 1080p formats are VP9/AV1 only
//! in practice, and webOS 10 refuses H.264 declared at 4K, so H.264 is only ever chosen up to
//! 1080p; above that VP9 or AV1. Audio: Opus (251) next to VP9/AV1, AAC (140) next to H.264,
//! each falling back to the other.

use crate::sabr::formats::{audio_codec, best_audio, video_codec, AudioCodec, VideoCodec};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Request {
    pub max_height: u32,
    /// Preferred codec; others are used when they reach a higher resolution (within the cap).
    pub codec: VideoCodec,
    /// Consider HDR (PQ/HLG) formats, and prefer them at equal height.
    pub allow_hdr: bool,
    /// Only formats with a direct `url` (ranged mode); false for SABR.
    pub need_url: bool,
}

#[derive(Clone, Debug)]
pub struct Choice {
    pub video: Value,
    pub audio: Option<Value>,
    pub video_codec: VideoCodec,
    pub audio_codec: Option<AudioCodec>,
}

impl Choice {
    pub fn video_itag(&self) -> i32 {
        self.video["itag"].as_i64().unwrap_or(0) as i32
    }
    pub fn audio_itag(&self) -> Option<i32> {
        self.audio.as_ref().and_then(|a| a["itag"].as_i64()).map(|i| i as i32)
    }
}

fn n(f: &Value, k: &str) -> u64 {
    f[k].as_u64().or_else(|| f[k].as_str().and_then(|s| s.parse().ok())).unwrap_or(0)
}

pub fn is_hdr(f: &Value) -> bool {
    f["colorInfo"]["transferCharacteristics"].as_str().is_some_and(|t| t.contains("2084") || t.contains("HLG"))
        || f["qualityLabel"].as_str().is_some_and(|q| q.contains("HDR"))
}

/// Highest cap a codec may be chosen at.
pub fn codec_cap(codec: VideoCodec, max_height: u32) -> u32 {
    match codec {
        VideoCodec::H264 => max_height.min(1080),
        _ => max_height,
    }
}

fn best_of(adaptive: &[Value], codec: VideoCodec, r: &Request) -> Option<Value> {
    let cap = codec_cap(codec, r.max_height) as u64;
    adaptive
        .iter()
        .filter(|f| video_codec(f) == Some(codec))
        .filter(|f| !r.need_url || f["url"].is_string())
        .filter(|f| r.allow_hdr || !is_hdr(f))
        .filter(|f| n(f, "height") <= cap)
        .max_by_key(|f| (n(f, "height"), n(f, "fps"), r.allow_hdr && is_hdr(f), n(f, "bitrate")))
        .cloned()
}

/// Picks the video and audio formats.
pub fn choose(adaptive: &[Value], r: Request) -> Option<Choice> {
    let mut order = vec![r.codec];
    let rest: &[VideoCodec] = if r.max_height > 1080 {
        &[VideoCodec::Vp9, VideoCodec::Av1, VideoCodec::H264]
    } else {
        &[VideoCodec::H264, VideoCodec::Vp9, VideoCodec::Av1]
    };
    for c in rest {
        if !order.contains(c) {
            order.push(*c);
        }
    }
    // Highest resolution wins; at equal height the earlier codec in `order` (the requested one
    // first). Frame rate only ranks formats within one codec.
    let mut best: Option<(u64, Value, VideoCodec)> = None;
    for c in &order {
        if let Some(f) = best_of(adaptive, *c, &r) {
            let h = n(&f, "height");
            if best.as_ref().map_or(true, |(bh, _, _)| h > *bh) {
                best = Some((h, f, *c));
            }
        }
    }
    let (_, video, vc) = best?;
    let prefs = match vc {
        VideoCodec::H264 => [AudioCodec::Aac, AudioCodec::Opus],
        _ => [AudioCodec::Opus, AudioCodec::Aac],
    };
    let audio = prefs.iter().find_map(|ac| {
        let usable: Vec<Value> = adaptive.iter().filter(|f| !r.need_url || f["url"].is_string()).cloned().collect();
        best_audio(&usable, *ac).cloned()
    });
    let audio_codec = audio.as_ref().and_then(audio_codec);
    Some(Choice { video, audio, video_codec: vc, audio_codec })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn fmts() -> Vec<Value> {
        vec![
            json!({"itag":137,"url":"u","mimeType":"video/mp4; codecs=\"avc1.640028\"","height":1080,"fps":30,"bitrate":4000000}),
            json!({"itag":299,"url":"u","mimeType":"video/mp4; codecs=\"avc1.64002a\"","height":1080,"fps":60,"bitrate":6000000}),
            json!({"itag":266,"url":"u","mimeType":"video/mp4; codecs=\"avc1.640033\"","height":2160,"fps":30,"bitrate":16000000}),
            json!({"itag":303,"url":"u","mimeType":"video/webm; codecs=\"vp9\"","height":1080,"fps":60,"bitrate":4000000}),
            json!({"itag":315,"url":"u","mimeType":"video/webm; codecs=\"vp9\"","height":2160,"fps":60,"bitrate":20000000}),
            json!({"itag":337,"url":"u","mimeType":"video/webm; codecs=\"vp09.02.51.10.01.09.16.09.00\"","height":2160,"fps":60,"bitrate":25000000,"colorInfo":{"transferCharacteristics":"COLOR_TRANSFER_CHARACTERISTICS_SMPTEST2084"}}),
            json!({"itag":401,"url":"u","mimeType":"video/mp4; codecs=\"av01.0.12M.08\"","height":2160,"fps":30,"bitrate":12000000}),
            json!({"itag":140,"url":"u","mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":130000}),
            json!({"itag":251,"url":"u","mimeType":"audio/webm; codecs=\"opus\"","bitrate":140000}),
        ]
    }

    fn req(max_height: u32, codec: VideoCodec) -> Request {
        Request { max_height, codec, allow_hdr: false, need_url: true }
    }

    #[test]
    fn h264_never_above_1080() {
        let c = choose(&fmts(), req(2160, VideoCodec::H264)).unwrap();
        // H.264 is capped at 1080p, so the 2160p VP9 wins on resolution.
        assert_eq!((c.video_itag(), c.audio_itag()), (315, Some(251)));
        let c = choose(&fmts(), req(1080, VideoCodec::H264)).unwrap();
        assert_eq!((c.video_itag(), c.audio_itag()), (299, Some(140)));
    }

    #[test]
    fn preferred_codec_wins_ties_and_av1_at_4k() {
        let c = choose(&fmts(), req(1080, VideoCodec::Vp9)).unwrap();
        assert_eq!(c.video_itag(), 303);
        // Equal height: the requested codec wins even at a lower frame rate.
        let c = choose(&fmts(), req(2160, VideoCodec::Av1)).unwrap();
        assert_eq!((c.video_itag(), c.audio_itag()), (401, Some(251)));
        // A higher resolution in another codec beats the preference.
        let c = choose(&fmts(), req(1440, VideoCodec::Av1)).unwrap();
        assert_eq!(c.video_itag(), 303);
    }

    #[test]
    fn hdr_only_when_allowed() {
        let mut r = req(2160, VideoCodec::Vp9);
        assert_eq!(choose(&fmts(), r).unwrap().video_itag(), 315);
        r.allow_hdr = true;
        assert_eq!(choose(&fmts(), r).unwrap().video_itag(), 337);
    }

    #[test]
    fn need_url_filters_sabr_only_formats() {
        let mut f = fmts();
        for x in &mut f {
            if x["itag"] == 299 || x["itag"] == 140 {
                x.as_object_mut().unwrap().remove("url");
            }
        }
        let c = choose(&f, req(1080, VideoCodec::H264)).unwrap();
        assert_eq!((c.video_itag(), c.audio_itag()), (137, Some(251)));
        let mut r = req(1080, VideoCodec::H264);
        r.need_url = false;
        assert_eq!(choose(&f, r).unwrap().video_itag(), 299);
    }
}
