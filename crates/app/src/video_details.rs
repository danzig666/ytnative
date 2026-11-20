//! "Video details" (what YouTube calls "Stats for nerds"): a small panel in the top-left corner
//! over the playing video telling which engine plays it, the formats it plays, how much is
//! buffered and how fast the connection is.
//!
//! The values come from what the engines already know: the formats they chose (engine B plays
//! exactly one video and one audio format; engine A hands the TV's player a manifest with a ladder
//! of H.264 formats and the player picks one, so the one playing is matched by the size the
//! decoder reports), the decoder's size, frame rate and HDR type, and two measurements engine B
//! makes anyway (how far its media reaches past the playhead, how fast its segments download).
//! Anything an engine does not know reads "unknown", never 0.
//!
//! Neither engine's pipeline reports decoded or dropped frames (Starfish's FRAMEREADY is a ~5 Hz
//! position tick, uMediaServer sends no frame counts), so the panel has no row for them.

use crate::player::Status;
use plx_ui::Rect;
use std::sync::Arc;

/// Which engine plays the video.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Engine {
    #[default]
    Unknown,
    /// Engine A: the TV's own DASH / HLS player (uMediaServer).
    System,
    /// Engine B: the app demuxes and feeds the TV's decoder itself.
    Feed,
    /// The Windows simulator: no media pipeline at all.
    #[cfg_attr(not(windows), allow(dead_code))]
    Simulator,
}

/// One video or audio format of the stream.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Format {
    /// The RFC 6381 codec string ("avc1.640028", "vp09.00.51.08", "opus").
    pub codecs: String,
    /// Bits per second (the average when YouTube gives one).
    pub bitrate: Option<u64>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub hdr: bool,
}

impl Format {
    /// From one of YouTube's adaptive formats.
    pub fn from_adaptive(f: &serde_json::Value) -> Format {
        let n = |k: &str| f[k].as_u64().or_else(|| f[k].as_str().and_then(|s| s.parse().ok()));
        Format {
            codecs: f["mimeType"].as_str().and_then(mime_codecs).unwrap_or_default(),
            bitrate: n("averageBitrate").or_else(|| n("bitrate")).filter(|b| *b > 0),
            width: n("width").unwrap_or(0) as u32,
            height: n("height").unwrap_or(0) as u32,
            fps: n("fps").unwrap_or(0) as u32,
            hdr: yt_core::demux::select::is_hdr(f),
        }
    }

    /// The quality rung (the short side: a vertical 1080p video is 1080x1920).
    fn rung(&self) -> u32 {
        if self.width > 0 { self.width.min(self.height) } else { self.height }
    }
}

/// `codecs="..."` of a mime type.
fn mime_codecs(mime: &str) -> Option<String> {
    let c = mime.split("codecs=").nth(1)?.trim_matches(|c| c == '"' || c == ' ');
    Some(c.trim_end_matches('"').to_string())
}

/// What the engine chose to play, set once per video.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Formats {
    pub engine: Engine,
    /// The video formats the engine plays from: one for engine B, engine A's whole ladder.
    pub video: Vec<Format>,
    pub audio: Vec<Format>,
}

impl Formats {
    /// Engine B's choice.
    pub fn chosen(engine: Engine, video: &serde_json::Value, audio: Option<&serde_json::Value>) -> Formats {
        Formats { engine, video: vec![Format::from_adaptive(video)], audio: audio.map(Format::from_adaptive).into_iter().collect() }
    }

    /// The representations of the DASH manifest engine A plays (`yt_core::mpd`'s output).
    pub fn from_mpd(engine: Engine, mpd: &str) -> Formats {
        let mut out = Formats { engine, ..Default::default() };
        for rep in mpd.split("<Representation ").skip(1) {
            let head = rep.split('>').next().unwrap_or("");
            let attr = |k: &str| head.split(&format!(" {k}=\"")).nth(1).or_else(|| head.strip_prefix(&format!("{k}=\""))).and_then(|r| r.split('"').next());
            let num = |k: &str| attr(k).and_then(|v| v.parse::<u64>().ok());
            let f = Format {
                codecs: attr("codecs").unwrap_or("").to_string(),
                bitrate: num("bandwidth").filter(|b| *b > 0),
                width: num("width").unwrap_or(0) as u32,
                height: num("height").unwrap_or(0) as u32,
                fps: num("frameRate").unwrap_or(0) as u32,
                hdr: false,
            };
            if attr("height").is_some() {
                out.video.push(f);
            } else if attr("audioSamplingRate").is_some() {
                out.audio.push(f);
            }
        }
        out
    }

    /// The video format playing, given the decoder's size and frame rate (0: not reported yet).
    pub fn current_video(&self, width: u32, height: u32, fps: u32) -> Option<&Format> {
        if self.video.len() == 1 {
            return self.video.first();
        }
        if height == 0 {
            return None;
        }
        let rung = if width > 0 { width.min(height) } else { height };
        let at: Vec<&Format> = self.video.iter().filter(|f| f.rung() == rung).collect();
        at.iter().find(|f| fps > 0 && f.fps == fps).or_else(|| at.first()).copied()
    }

    /// The audio format playing, when there is only one it can be.
    pub fn current_audio(&self) -> Option<&Format> {
        (self.audio.len() == 1).then(|| &self.audio[0])
    }
}

/// What the panel shows, kept with the player's status.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Details {
    pub formats: Arc<Formats>,
    /// How far (milliseconds of video) the media at hand reaches past the playhead.
    pub buffered_ms: Option<u64>,
    /// The download speed (bits per second) measured on the last video segments.
    pub speed_bps: Option<u64>,
}

/// Segments smaller than this say more about the connection's latency than its speed.
const SPEED_MIN_BYTES: u64 = 256 << 10;

/// The connection speed after a segment of `bytes` took `ms` to download: a moving average, so
/// one slow or fast segment does not swing it.
pub fn blend_speed(prev: Option<u64>, bytes: u64, ms: u64) -> Option<u64> {
    if bytes < SPEED_MIN_BYTES || ms == 0 {
        return prev;
    }
    let sample = bytes * 8 * 1000 / ms;
    Some(prev.map_or(sample, |p| (p * 7 + sample * 3) / 10))
}

/// Milliseconds from `playhead_ns` to the furthest of `reaches_ns` (positions the media at hand
/// reaches; 0 or less: nothing there). `None` when nothing is at hand.
pub fn ahead_ms(playhead_ns: i64, reaches_ns: &[i64]) -> Option<u64> {
    let end = reaches_ns.iter().copied().filter(|r| *r > 0).max()?;
    Some(((end - playhead_ns).max(0) / 1_000_000) as u64)
}

/// The product name of a codec string.
pub fn codec_name(codecs: &str) -> &str {
    let c = codecs.to_ascii_lowercase();
    match () {
        _ if c.starts_with("avc1") || c.starts_with("avc3") => "H.264",
        _ if c.starts_with("vp09") || c.starts_with("vp9") => "VP9",
        _ if c.starts_with("av01") => "AV1",
        _ if c.starts_with("hev1") || c.starts_with("hvc1") => "H.265",
        _ if c.starts_with("mp4a") => "AAC",
        _ if c.starts_with("opus") => "Opus",
        _ if c.starts_with("ec-3") => "Dolby Digital Plus",
        _ if c.starts_with("ac-3") => "Dolby Digital",
        _ => codecs,
    }
}

/// A value on the panel; words are translated when drawn.
#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Unknown,
    /// Shown as it is (codec names, sizes, the video id).
    Raw(String),
    /// A phrase from the translation table.
    Phrase(&'static str),
    /// A translated template with `{}` replaced by the number.
    Template(&'static str, String),
}

fn rate(bps: u64) -> Val {
    if bps >= 1_000_000 {
        Val::Template("{} megabits per second", format!("{:.1}", bps as f64 / 1e6))
    } else {
        Val::Template("{} kilobits per second", format!("{}", (bps + 500) / 1000))
    }
}

fn codec(f: Option<&Format>, all: &[Format]) -> Val {
    // When the playing one is not known, the codec still is if every candidate shares it.
    let f = f.or_else(|| all.first().filter(|first| all.iter().all(|o| codec_name(&o.codecs) == codec_name(&first.codecs))));
    match f.filter(|f| !f.codecs.is_empty()) {
        Some(f) if codec_name(&f.codecs) != f.codecs => Val::Raw(format!("{} ({})", codec_name(&f.codecs), f.codecs)),
        Some(f) => Val::Raw(f.codecs.clone()),
        None => Val::Unknown,
    }
}

/// The panel's rows: (label, value).
pub fn rows(st: &Status) -> Vec<(&'static str, Val)> {
    let d = &st.details;
    let fm = &d.formats;
    let video = fm.current_video(st.width, st.height, st.fps);
    let audio = fm.current_audio();
    let engine = match fm.engine {
        Engine::Unknown => Val::Unknown,
        Engine::System => Val::Phrase("System player"),
        Engine::Feed => Val::Phrase("Engine B"),
        Engine::Simulator => Val::Phrase("Simulator, no video"),
    };
    let (w, h) = match (st.width, st.height, video) {
        (w, h, _) if w > 0 && h > 0 => (w, h),
        (_, _, Some(f)) => (f.width, f.height),
        _ => (0, 0),
    };
    let size = if w > 0 && h > 0 { Val::Raw(format!("{w} \u{00D7} {h}")) } else { Val::Unknown };
    let fps = match st.fps {
        0 => video.map_or(0, |f| f.fps),
        f => f,
    };
    let fps = if fps > 0 { Val::Template("{} frames per second", fps.to_string()) } else { Val::Unknown };
    let hdr = match st.hdr.as_str() {
        "" => match video {
            Some(f) if f.hdr => Val::Raw("HDR".into()),
            Some(_) => Val::Phrase("None"),
            None => Val::Unknown,
        },
        h if h.eq_ignore_ascii_case("none") => Val::Phrase("None"),
        h => Val::Raw(h.to_string()),
    };
    vec![
        ("Playback engine", engine),
        ("Video codec", codec(video, &fm.video)),
        ("Resolution", size),
        ("Frame rate", fps),
        ("Video bitrate", video.and_then(|f| f.bitrate).map_or(Val::Unknown, rate)),
        ("Audio codec", codec(audio, &fm.audio)),
        ("Audio bitrate", audio.and_then(|f| f.bitrate).map_or(Val::Unknown, rate)),
        ("HDR", hdr),
        ("Buffered ahead", d.buffered_ms.map_or(Val::Unknown, |ms| Val::Template("{} seconds", format!("{:.1}", ms as f64 / 1000.0)))),
        ("Connection speed", d.speed_bps.map_or(Val::Unknown, rate)),
        ("Video ID", if st.video_id.is_empty() { Val::Unknown } else { Val::Raw(st.video_id.clone()) }),
    ]
}

const X: f32 = 40.0;
const W: f32 = 700.0;
const PAD: f32 = 20.0;
const ROW: f32 = 36.0;
const SIZE: i32 = 24;
const LABEL_W: f32 = 270.0;

/// The panel shows for this long after the last key press (or the start of the video), then
/// fades out until the next key: on an OLED panel its box and labels must not stay put for the
/// whole video. Any key brings it back.
pub const SHOW_MS: u32 = 15_000;
/// How long the fade out takes.
pub const FADE_MS: u32 = 1_000;

/// The panel's opacity `quiet_ms` after the last key press: whole for [`SHOW_MS`], then down to
/// nothing over [`FADE_MS`].
pub fn opacity(quiet_ms: u32) -> f32 {
    match quiet_ms.checked_sub(SHOW_MS) {
        None => 1.0,
        Some(t) if t >= FADE_MS => 0.0,
        Some(t) => 1.0 - t as f32 / FADE_MS as f32,
    }
}

/// Draws the panel with its top at `top` (the caller keeps it clear of the controls) at opacity
/// `alpha` ([`opacity`]). Under the player's [`crate::ui::HdrDim`] and its screen dimming, like
/// the other player overlays.
pub fn draw(p: plx_ui::Painter, m: &dyn plx_machine::machine::Measure, st: &Status, top: f32, alpha: f32) {
    use crate::ui;
    let fade = |c: [f32; 4]| [c[0], c[1], c[2], c[3] * alpha];
    let rows = rows(st);
    let h = PAD * 2.0 + 44.0 + rows.len() as f32 * ROW;
    ui::fill(p, Rect::new(X, top, W, h), 12.0, fade([0.0, 0.0, 0.0, 0.6]));
    ui::text(p, "Video details", X + PAD, top + PAD, 26, fade(ui::WHITE), ui::ALIGN_LEFT, true);
    let vx = X + PAD + LABEL_W;
    let vw = W - PAD * 2.0 - LABEL_W;
    for (i, (label, val)) in rows.iter().enumerate() {
        let y = top + PAD + 44.0 + i as f32 * ROW;
        ui::text_cut(p, m, crate::i18n::tr(label), X + PAD, y, LABEL_W - 12.0, SIZE, fade(ui::DIM), ui::ALIGN_LEFT, false);
        let (s, ink) = match val {
            Val::Unknown => (crate::i18n::tr("unknown").to_string(), ui::FAINT),
            Val::Raw(s) => (s.clone(), ui::WHITE),
            Val::Phrase(s) => (crate::i18n::tr(s).to_string(), ui::WHITE),
            Val::Template(t, n) => (crate::i18n::tr(t).replace("{}", n), ui::WHITE),
        };
        ui::text_cut(p, m, &s, vx, y, vw, SIZE, fade(ink), ui::ALIGN_LEFT, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MPD: &str = r#"<AdaptationSet id="0" contentType="video">
<Representation id="137" codecs="avc1.640028" bandwidth="4400000" width="1920" height="1080" frameRate="30"><BaseURL>x</BaseURL></Representation>
<Representation id="299" codecs="avc1.64002a" bandwidth="6100000" width="1920" height="1080" frameRate="60"><BaseURL>x</BaseURL></Representation>
<Representation id="136" codecs="avc1.4d401f" bandwidth="2300000" width="1280" height="720" frameRate="30"><BaseURL>x</BaseURL></Representation>
</AdaptationSet><AdaptationSet id="1" contentType="audio">
<Representation id="140" codecs="mp4a.40.2" bandwidth="130000" audioSamplingRate="44100"><BaseURL>a</BaseURL></Representation>
</AdaptationSet>"#;

    #[test]
    fn the_manifest_ladder_is_read_and_the_playing_rung_matched() {
        let f = Formats::from_mpd(Engine::System, MPD);
        assert_eq!((f.video.len(), f.audio.len()), (3, 1));
        assert_eq!(f.audio[0], Format { codecs: "mp4a.40.2".into(), bitrate: Some(130000), ..Default::default() });
        assert_eq!(f.current_video(1280, 720, 30).map(|v| v.bitrate), Some(Some(2300000)));
        assert_eq!(f.current_video(1920, 1080, 60).map(|v| v.codecs.as_str()), Some("avc1.64002a"));
        assert_eq!(f.current_video(1920, 1080, 0).map(|v| v.codecs.as_str()), Some("avc1.640028"));
        assert_eq!(f.current_video(0, 0, 0), None, "not reported yet");
        assert_eq!(f.current_video(854, 480, 30), None, "a size the ladder does not have");
    }

    #[test]
    fn an_adaptive_format_gives_its_codec_average_bitrate_and_hdr() {
        let v = json!({"itag": 337, "mimeType": "video/webm; codecs=\"vp09.02.51.10.01.09.16.09.00\"", "bitrate": 30000000, "averageBitrate": "21000000",
                       "width": 3840, "height": 2160, "fps": 60, "colorInfo": {"transferCharacteristics": "COLOR_TRANSFER_CHARACTERISTICS_SMPTEST2084"}});
        let a = json!({"itag": 251, "mimeType": "audio/webm; codecs=\"opus\"", "bitrate": 140000});
        let f = Formats::chosen(Engine::Feed, &v, Some(&a));
        let fv = f.current_video(0, 0, 0).unwrap();
        assert_eq!((fv.codecs.as_str(), fv.bitrate, fv.fps, fv.hdr), ("vp09.02.51.10.01.09.16.09.00", Some(21000000), 60, true));
        assert_eq!(f.current_audio().unwrap().codecs, "opus");
    }

    #[test]
    fn unknown_values_read_unknown_not_zero() {
        let st = Status::default();
        for (label, v) in rows(&st) {
            assert_eq!(v, Val::Unknown, "{label}");
        }
    }

    #[test]
    fn rows_for_engine_b() {
        let v = json!({"mimeType": "video/webm; codecs=\"vp09.00.51.08\"", "bitrate": 12345678, "width": 3840, "height": 2160, "fps": 30});
        let a = json!({"mimeType": "audio/webm; codecs=\"opus\"", "bitrate": 128000});
        let st = Status {
            video_id: "abc".into(),
            width: 3840,
            height: 2160,
            fps: 30,
            hdr: "none".into(),
            details: Details { formats: Arc::new(Formats::chosen(Engine::Feed, &v, Some(&a))), buffered_ms: Some(12_345), speed_bps: Some(85_000_000) },
            ..Default::default()
        };
        let r: std::collections::HashMap<_, _> = rows(&st).into_iter().collect();
        assert_eq!(r["Playback engine"], Val::Phrase("Engine B"));
        assert_eq!(r["Video codec"], Val::Raw("VP9 (vp09.00.51.08)".into()));
        assert_eq!(r["Resolution"], Val::Raw("3840 \u{00D7} 2160".into()));
        assert_eq!(r["Frame rate"], Val::Template("{} frames per second", "30".into()));
        assert_eq!(r["Video bitrate"], Val::Template("{} megabits per second", "12.3".into()));
        assert_eq!(r["Audio codec"], Val::Raw("Opus (opus)".into()));
        assert_eq!(r["Audio bitrate"], Val::Template("{} kilobits per second", "128".into()));
        assert_eq!(r["HDR"], Val::Phrase("None"));
        assert_eq!(r["Buffered ahead"], Val::Template("{} seconds", "12.3".into()));
        assert_eq!(r["Connection speed"], Val::Template("{} megabits per second", "85.0".into()));
        assert_eq!(r["Video ID"], Val::Raw("abc".into()));
    }

    #[test]
    fn engine_a_knows_the_codec_before_the_decoder_reports_but_not_the_bitrate() {
        let st = Status { details: Details { formats: Arc::new(Formats::from_mpd(Engine::System, MPD)), ..Default::default() }, ..Default::default() };
        let r: std::collections::HashMap<_, _> = rows(&st).into_iter().collect();
        assert_eq!(r["Video codec"], Val::Raw("H.264 (avc1.640028)".into()));
        assert_eq!(r["Video bitrate"], Val::Unknown);
        assert_eq!(r["Audio bitrate"], Val::Template("{} kilobits per second", "130".into()));
        assert_eq!(r["HDR"], Val::Unknown);
    }

    #[test]
    fn speed_is_averaged_and_small_segments_are_ignored() {
        assert_eq!(blend_speed(None, 1000, 10), None);
        assert_eq!(blend_speed(None, 1_000_000, 0), None);
        assert_eq!(blend_speed(None, 1_000_000, 1000), Some(8_000_000));
        assert_eq!(blend_speed(Some(8_000_000), 2_000_000, 1000), Some(10_400_000));
        assert_eq!(blend_speed(Some(5), 100, 1), Some(5));
    }

    #[test]
    fn buffered_ahead_is_the_furthest_reach() {
        assert_eq!(ahead_ms(10_000_000_000, &[]), None);
        assert_eq!(ahead_ms(10_000_000_000, &[0]), None, "an empty lane reaches nowhere");
        assert_eq!(ahead_ms(10_000_000_000, &[15_000_000_000, 22_500_000_000]), Some(12_500));
        assert_eq!(ahead_ms(10_000_000_000, &[9_000_000_000]), Some(0));
    }

    #[test]
    fn the_panel_fades_out_after_a_while_without_keys() {
        assert_eq!(opacity(0), 1.0);
        assert_eq!(opacity(SHOW_MS), 1.0);
        assert!((opacity(SHOW_MS + FADE_MS / 2) - 0.5).abs() < 0.01);
        assert_eq!(opacity(SHOW_MS + FADE_MS), 0.0);
        assert_eq!(opacity(u32::MAX), 0.0);
    }

    #[test]
    fn codec_names() {
        assert_eq!(codec_name("av01.0.12M.10"), "AV1");
        assert_eq!(codec_name("ec-3"), "Dolby Digital Plus");
        assert_eq!(codec_name("xyz"), "xyz");
    }
}
