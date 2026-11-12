//! The StarfishMediaAPIs BUFFERSTREAM Load payload. Pure (std only), so it is unit-tested on the
//! host with `rustc --test` (see docs/feed-engine.md).
//!
//! Template and rules from plx-native (`rust-modules/play-feed/src/lib.rs`, GPL-3.0-or-later):
//! - `esInfo.pauseAtDecodeTime` MUST be false (true + no decode-time trigger = no frame decoded).
//! - Sink envelope (`adaptiveStreaming.maxWidth/maxHeight/maxFrameRate`): webOS 10 allocates the
//!   decoder against the DECLARED ceiling; H.264 declared at 3840x2160@60 is refused with
//!   `type=18 num=601 Resource Allocation Error`. So H.264 declares 1920x1080 when the stream fits
//!   FHD (≤ 1920x1088), and its frame-rate class (24/25/30/50/60): a 24p stream declared @60 was
//!   presented at 13 fps on webOS 4.10. VP9/AV1 keep 3840x2160@60 (HEVC's verified rule; not
//!   measured for these codecs).
//! - Opus needs `contents.opusInfo {channels, sampleRate:48, streamHeader: base64(OpusHead)}`.
//! - `option.windowId` (webOS 5+ exported window) is the whole video-plane binding.

/// Codec names the pipeline knows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VideoDecl {
    H264,
    Vp9,
    Av1,
}

impl VideoDecl {
    pub fn name(self) -> &'static str {
        match self {
            VideoDecl::H264 => "H264",
            VideoDecl::Vp9 => "VP9",
            VideoDecl::Av1 => "AV1",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AudioDecl {
    Aac,
    /// Opus with its channel count and RFC 7845 identification header.
    Opus { channels: u8, head: Vec<u8> },
}

#[derive(Clone, Debug)]
pub struct Spec {
    pub app_id: String,
    pub window_id: Option<String>,
    pub video: VideoDecl,
    pub width: u32,
    pub height: u32,
    /// Real frame rate (0 = unknown).
    pub fps: f64,
    pub audio: Option<AudioDecl>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Envelope {
    pub w: u32,
    pub h: u32,
    pub fps: u32,
}

/// plx-native `PAYLOAD_AV`, provider renamed. Keys are spliced by exact-match replacement, so the
/// key order stays byte-identical to what plx-native verified on devices.
const TEMPLATE: &str = r#"{"args":[{"mediaTransportType":"BUFFERSTREAM","option":{"appId":"@APPID@","externalStreamingInfo":{"contents":{"codec":{"video":"H264","audio":"AC3"},"esInfo":{"pauseAtDecodeTime":false,"ptsToDecode":0,"seperatedPTS":true},"format":"RAW","provider":"ytnative"},"streamQualityInfo":true,"streamQualityInfoNonFlushable":true,"audioSync":true,"restartStreaming":false,"bufferingCtrInfo":{"bufferMaxLevel":0,"bufferMinLevel":0,"preBufferByte":0,"qBufferLevelAudio":0,"qBufferLevelVideo":0,"srcBufferLevelAudio":{"minimum":1,"maximum":1048576},"srcBufferLevelVideo":{"minimum":1,"maximum":8388608}}},"needAudio":true,"queryPosition":false,"lowDelayMode":false,"transmission":{"contentsType":"LIVE"},"adaptiveStreaming":{"audioOnly":false,"maxWidth":1920,"maxHeight":1080,"maxFrameRate":30}}}]}"#;

/// The broadcast rate class a stream belongs to (24/25/30/50/60), tolerant of a 1 ms container
/// timebase (30.303 → 30); else the rate rounded up.
pub fn fps_class(fps: f64) -> u32 {
    for class in [24u32, 25, 30, 50, 60] {
        let c = f64::from(class);
        if fps <= c + 0.01 || 1000.0 / fps > 1000.0 / c - 1.0 {
            return class;
        }
    }
    fps.ceil() as u32
}

/// (num, den) for `esInfo.videoFpsValue/videoFpsScale`.
pub fn fps_rational(fps: f64) -> Option<(i64, i64)> {
    if !(fps > 0.0 && fps.is_finite()) {
        return None;
    }
    let near = |a: f64, tol: f64| (fps - a).abs() < tol;
    Some(if near(23.976, 0.01) {
        (24000, 1001)
    } else if near(29.97, 0.01) {
        (30000, 1001)
    } else if near(59.94, 0.02) {
        (60000, 1001)
    } else if near(47.952, 0.02) {
        (48000, 1001)
    } else if (fps - fps.round()).abs() < 0.001 {
        (fps.round() as i64, 1)
    } else {
        ((fps * 1000.0).round() as i64, 1000)
    })
}

/// The declared sink envelope (see the module doc).
pub fn sink_envelope(video: VideoDecl, width: u32, height: u32, fps: f64) -> Envelope {
    let fits_fhd = width > 0 && height > 0 && width <= 1920 && height <= 1088;
    let mut env = if video == VideoDecl::H264 && fits_fhd {
        Envelope { w: 1920, h: 1080, fps: 60 }
    } else {
        Envelope { w: 3840, h: 2160, fps: 60 }
    };
    if video == VideoDecl::H264 && fps > 0.0 && fps.is_finite() {
        env.fps = fps_class(fps).min(60);
    }
    env
}

pub fn base64(b: &[u8]) -> String {
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut s = String::with_capacity(b.len().div_ceil(3) * 4);
    for c in b.chunks(3) {
        let n = (c[0] as u32) << 16 | (*c.get(1).unwrap_or(&0) as u32) << 8 | *c.get(2).unwrap_or(&0) as u32;
        s.push(T[(n >> 18) as usize & 63] as char);
        s.push(T[(n >> 12) as usize & 63] as char);
        s.push(if c.len() > 1 { T[(n >> 6) as usize & 63] as char } else { '=' });
        s.push(if c.len() > 2 { T[n as usize & 63] as char } else { '=' });
    }
    s
}

fn json_escape(s: &str) -> String {
    let mut o = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => o.push_str("\\\""),
            '\\' => o.push_str("\\\\"),
            c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
            c => o.push(c),
        }
    }
    o
}

/// The complete Load payload.
pub fn build(spec: &Spec) -> String {
    let env = sink_envelope(spec.video, spec.width, spec.height, spec.fps);
    let mut p = TEMPLATE.replace(r#""video":"H264""#, &format!(r#""video":"{}""#, spec.video.name()));
    p = p
        .replace(r#""maxWidth":1920"#, &format!(r#""maxWidth":{}"#, env.w))
        .replace(r#""maxHeight":1080"#, &format!(r#""maxHeight":{}"#, env.h))
        .replace(r#""maxFrameRate":30"#, &format!(r#""maxFrameRate":{}"#, env.fps));
    match &spec.audio {
        Some(AudioDecl::Aac) => p = p.replace(r#""audio":"AC3""#, r#""audio":"AAC""#),
        Some(AudioDecl::Opus { channels, head }) => {
            p = p.replace(r#""audio":"AC3""#, r#""audio":"OPUS""#);
            let anchor = r#""provider":"ytnative""#;
            p = p.replace(
                anchor,
                &format!(r#"{anchor},"opusInfo":{{"channels":{channels},"sampleRate":48,"streamHeader":"{}"}}"#, base64(head)),
            );
        }
        None => {
            p = p
                .replace(r#","audio":"AC3""#, "")
                .replace(r#""needAudio":true"#, r#""needAudio":false"#);
        }
    }
    if let Some((num, den)) = fps_rational(spec.fps) {
        p = p
            .replace(r#""seperatedPTS":true}"#, &format!(r#""seperatedPTS":true,"videoFpsValue":{num},"videoFpsScale":{den}}}"#))
            .replace(r#""audioOnly":false"#, r#""audioOnly":false,"adaptiveResolution":true"#);
    }
    p = p.replace("@APPID@", &json_escape(&spec.app_id));
    if let Some(w) = spec.window_id.as_deref().filter(|w| !w.is_empty()) {
        let anchor = format!(r#""appId":"{}""#, json_escape(&spec.app_id));
        p = p.replacen(&anchor, &format!(r#"{anchor},"windowId":"{}""#, json_escape(w)), 1);
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(video: VideoDecl, w: u32, h: u32, fps: f64, audio: Option<AudioDecl>) -> Spec {
        Spec { app_id: "com.example.yt".into(), window_id: Some("_Window_Id_3".into()), video, width: w, height: h, fps, audio }
    }

    #[test]
    fn h264_1080p_declares_fhd_and_rate_class() {
        let p = build(&spec(VideoDecl::H264, 1920, 1080, 59.94, Some(AudioDecl::Aac)));
        assert!(p.contains(r#""codec":{"video":"H264","audio":"AAC"}"#), "{p}");
        assert!(p.contains(r#""maxWidth":1920,"maxHeight":1080,"maxFrameRate":60"#), "{p}");
        assert!(p.contains(r#""pauseAtDecodeTime":false"#));
        assert!(p.contains(r#""videoFpsValue":60000,"videoFpsScale":1001"#), "{p}");
        assert!(p.contains(r#""adaptiveResolution":true"#));
        assert!(p.contains(r#""appId":"com.example.yt","windowId":"_Window_Id_3""#), "{p}");
        assert!(p.contains(r#""needAudio":true"#));
        let p = build(&spec(VideoDecl::H264, 1280, 720, 30.0, Some(AudioDecl::Aac)));
        assert!(p.contains(r#""maxWidth":1920,"maxHeight":1080,"maxFrameRate":30"#), "{p}");
        // Coded 1088 still fits FHD.
        assert_eq!(sink_envelope(VideoDecl::H264, 1920, 1088, 24.0), Envelope { w: 1920, h: 1080, fps: 24 });
        // Larger than FHD (never chosen by the selector, but declared truthfully).
        assert_eq!(sink_envelope(VideoDecl::H264, 2560, 1440, 30.0), Envelope { w: 3840, h: 2160, fps: 30 });
    }

    #[test]
    fn vp9_av1_declare_uhd60_and_opus_info() {
        let head = b"OpusHead\x01\x02\x38\x01\x80\xbb\x00\x00\x00\x00\x00".to_vec();
        let p = build(&spec(VideoDecl::Vp9, 3840, 2160, 60.0, Some(AudioDecl::Opus { channels: 2, head: head.clone() })));
        assert!(p.contains(r#""codec":{"video":"VP9","audio":"OPUS"}"#), "{p}");
        assert!(p.contains(r#""maxWidth":3840,"maxHeight":2160,"maxFrameRate":60"#), "{p}");
        assert!(p.contains(&format!(r#""provider":"ytnative","opusInfo":{{"channels":2,"sampleRate":48,"streamHeader":"{}"}}"#, base64(&head))), "{p}");
        let p = build(&spec(VideoDecl::Av1, 1920, 1080, 24.0, Some(AudioDecl::Aac)));
        assert!(p.contains(r#""video":"AV1""#) && p.contains(r#""maxWidth":3840"#), "{p}");
    }

    #[test]
    fn video_only_and_no_window() {
        let mut s = spec(VideoDecl::H264, 1920, 1080, 0.0, None);
        s.window_id = None;
        let p = build(&s);
        assert!(p.contains(r#""codec":{"video":"H264"}"#), "{p}");
        assert!(p.contains(r#""needAudio":false"#));
        assert!(!p.contains("windowId") && !p.contains("videoFpsValue"));
        // Unknown rate: H.264 keeps 60.
        assert!(p.contains(r#""maxFrameRate":60"#));
    }

    #[test]
    fn helpers() {
        assert_eq!(base64(b"OpusHead"), "T3B1c0hlYWQ=");
        assert_eq!(base64(b"ab"), "YWI=");
        assert_eq!(fps_class(23.976), 24);
        assert_eq!(fps_class(30.303), 30);
        assert_eq!(fps_class(50.0), 50);
        assert_eq!(fps_class(59.94), 60);
        assert_eq!(fps_rational(25.0), Some((25, 1)));
        assert_eq!(fps_rational(0.0), None);
    }

    #[test]
    fn payload_is_valid_json_shape() {
        // Balanced braces/brackets and quotes (no serde in this std-only module).
        let p = build(&spec(VideoDecl::Vp9, 3840, 2160, 30.0, Some(AudioDecl::Opus { channels: 2, head: vec![1, 2, 3] })));
        let (mut depth, mut in_str, mut esc) = (0i32, false, false);
        for c in p.chars() {
            if in_str {
                match (esc, c) {
                    (true, _) => esc = false,
                    (false, '\\') => esc = true,
                    (false, '"') => in_str = false,
                    _ => {}
                }
                continue;
            }
            match c {
                '"' => in_str = true,
                '{' | '[' => depth += 1,
                '}' | ']' => depth -= 1,
                _ => {}
            }
            assert!(depth >= 0);
        }
        assert_eq!(depth, 0);
        assert!(!in_str);
    }
}
