//! Format selection over a `/player` response's `streamingData.adaptiveFormats` (works for SABR-only
//! responses too: formats carry itag/lastModified/xtags even when they have no `url`).

use super::proto::FormatId;
use serde_json::Value;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Track {
    Video,
    Audio,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoCodec {
    H264,
    Vp9,
    Av1,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioCodec {
    Aac,
    Opus,
}

/// One adaptive format as SABR needs it.
#[derive(Debug, Clone)]
pub struct SabrFormat {
    pub id: FormatId,
    pub track: Track,
    /// e.g. `video/mp4; codecs="avc1.640028"`.
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate: u64,
    pub approx_duration_ms: u64,
    pub audio_track_id: Option<String>,
    pub is_drc: bool,
}

impl SabrFormat {
    pub fn from_json(f: &Value) -> Option<SabrFormat> {
        let mime = f["mimeType"].as_str()?.to_string();
        let track = if mime.starts_with("video/") {
            Track::Video
        } else if mime.starts_with("audio/") {
            Track::Audio
        } else {
            return None;
        };
        let num = |k: &str| f[k].as_u64().or_else(|| f[k].as_str().and_then(|s| s.parse().ok()));
        Some(SabrFormat {
            id: FormatId {
                itag: f["itag"].as_i64()? as i32,
                lmt: num("lastModified").unwrap_or(0),
                xtags: f["xtags"].as_str().map(str::to_string),
            },
            track,
            mime_type: mime,
            width: num("width").unwrap_or(0) as u32,
            height: num("height").unwrap_or(0) as u32,
            fps: num("fps").unwrap_or(0) as u32,
            bitrate: num("bitrate").unwrap_or(0),
            approx_duration_ms: num("approxDurationMs").unwrap_or(0),
            audio_track_id: f["audioTrack"]["id"].as_str().map(str::to_string),
            is_drc: f["isDrc"].as_bool().unwrap_or(false),
        })
    }

    pub fn itag(&self) -> i32 {
        self.id.itag
    }

    /// True for WebM (VP9/Opus) formats, false for MP4.
    pub fn is_webm(&self) -> bool {
        self.mime_type.contains("/webm")
    }
}

fn codecs(f: &Value) -> &str {
    f["mimeType"].as_str().unwrap_or("")
}

pub fn video_codec(f: &Value) -> Option<VideoCodec> {
    let m = codecs(f);
    if !m.starts_with("video/") {
        return None;
    }
    if m.contains("avc1") {
        Some(VideoCodec::H264)
    } else if m.contains("vp9") || m.contains("vp09") {
        Some(VideoCodec::Vp9)
    } else if m.contains("av01") {
        Some(VideoCodec::Av1)
    } else {
        None
    }
}

pub fn audio_codec(f: &Value) -> Option<AudioCodec> {
    let m = codecs(f);
    if !m.starts_with("audio/") {
        return None;
    }
    if m.contains("mp4a") {
        Some(AudioCodec::Aac)
    } else if m.contains("opus") {
        Some(AudioCodec::Opus)
    } else {
        None
    }
}

fn is_hdr(f: &Value) -> bool {
    f["colorInfo"]["transferCharacteristics"].as_str().is_some_and(|t| t.contains("2084") || t.contains("HLG"))
        || f["qualityLabel"].as_str().is_some_and(|q| q.contains("HDR"))
}

fn n(f: &Value, k: &str) -> u64 {
    f[k].as_u64().or_else(|| f[k].as_str().and_then(|s| s.parse().ok())).unwrap_or(0)
}

/// Best SDR video of `codec` with height ≤ `max_height` (by height, then fps, then bitrate).
pub fn best_video(adaptive: &[Value], codec: VideoCodec, max_height: u32) -> Option<&Value> {
    adaptive
        .iter()
        .filter(|f| video_codec(f) == Some(codec) && !is_hdr(f) && crate::mpd::rung(f) <= max_height as u64)
        .max_by_key(|f| (crate::mpd::rung(f), n(f, "fps"), n(f, "bitrate")))
}

/// Best audio of `codec`: non-DRC, the default (original) language track when tracks are labelled,
/// highest bitrate.
pub fn best_audio(adaptive: &[Value], codec: AudioCodec) -> Option<&Value> {
    let any_default = adaptive.iter().any(|f| f["audioTrack"]["audioIsDefault"].as_bool() == Some(true));
    adaptive
        .iter()
        .filter(|f| audio_codec(f) == Some(codec) && !f["isDrc"].as_bool().unwrap_or(false))
        .filter(|f| !any_default || f["audioTrack"]["audioIsDefault"].as_bool() == Some(true))
        .max_by_key(|f| n(f, "bitrate"))
}

/// Finds a format by itag, preferring the non-DRC default audio track when an itag repeats.
pub fn by_itag(adaptive: &[Value], itag: i32) -> Option<&Value> {
    let mut c: Vec<&Value> = adaptive.iter().filter(|f| f["itag"].as_i64() == Some(itag as i64)).collect();
    c.sort_by_key(|f| {
        (
            f["isDrc"].as_bool().unwrap_or(false),
            f["audioTrack"]["audioIsDefault"].as_bool() == Some(false),
        )
    });
    c.first().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn selection() {
        let a = vec![
            json!({"itag":137,"mimeType":"video/mp4; codecs=\"avc1.640028\"","height":1080,"fps":30,"bitrate":4000000,"lastModified":"1"}),
            json!({"itag":299,"mimeType":"video/mp4; codecs=\"avc1.64002a\"","height":1080,"fps":60,"bitrate":6000000,"lastModified":"1"}),
            json!({"itag":264,"mimeType":"video/mp4; codecs=\"avc1.640032\"","height":1440,"fps":30,"bitrate":9000000,"lastModified":"1"}),
            json!({"itag":248,"mimeType":"video/webm; codecs=\"vp9\"","height":1080,"fps":30,"bitrate":3000000,"lastModified":"1"}),
            json!({"itag":140,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":130000,"lastModified":"1"}),
            json!({"itag":140,"mimeType":"audio/mp4; codecs=\"mp4a.40.2\"","bitrate":130000,"isDrc":true,"lastModified":"1"}),
            json!({"itag":251,"mimeType":"audio/webm; codecs=\"opus\"","bitrate":140000,"lastModified":"1"}),
        ];
        assert_eq!(best_video(&a, VideoCodec::H264, 1080).unwrap()["itag"], 299);
        assert_eq!(best_video(&a, VideoCodec::Vp9, 1080).unwrap()["itag"], 248);
        assert!(best_video(&a, VideoCodec::Av1, 1080).is_none());
        let aac = best_audio(&a, AudioCodec::Aac).unwrap();
        assert_eq!(aac["itag"], 140);
        assert!(aac["isDrc"].is_null());
        assert_eq!(best_audio(&a, AudioCodec::Opus).unwrap()["itag"], 251);
        assert!(by_itag(&a, 140).unwrap()["isDrc"].is_null());
        let f = SabrFormat::from_json(&a[0]).unwrap();
        assert_eq!((f.track, f.height, f.id.lmt), (Track::Video, 1080, 1));
    }
}
