//! What this TV can show and decode, read once on the first start so the playback settings begin
//! at the best the set supports (4K, VP9 / AV1, HDR) instead of 1080p H.264.
//!
//! Sources: the panel (`tv.systemproperty` UHD, `config` panelResolution / 8K / supportHDR) and
//! the media server's decoder list (`/etc/umediaserver/device_codec_capability_config.json`).
//! The services refuse developer-mode apps on webOS 23+, so the decoder list (readable by any
//! app) is what usually decides.

use crate::settings::{Codec, Settings};

/// The TV's media server lists every hardware decoder with its largest picture and frame rate.
#[cfg(not(windows))]
const CODEC_FILE: &str = "/etc/umediaserver/device_codec_capability_config.json";

#[derive(Debug, Clone, PartialEq)]
pub struct Caps {
    /// Panel height (1080, 2160, 4320).
    pub panel_height: u32,
    pub hdr: bool,
    /// Largest height the AV1 / VP9 decoders take at 50 fps or more (0: none).
    pub av1_height: u32,
    pub vp9_height: u32,
}

/// The settings a TV with these capabilities starts with: `(max_height, codec, hdr, engine B)`.
pub fn best(c: &Caps) -> (u32, Codec, bool, bool) {
    let panel = c.panel_height.min(2160);
    // A Full HD set keeps H.264 through the system player (it plays 1080p fine).
    let (codec, h) = if panel <= 1080 {
        (Codec::Avc, 1080)
    } else if c.av1_height >= panel {
        (Codec::Av1, panel)
    } else if c.vp9_height >= panel {
        (Codec::Vp9, panel)
    } else if c.av1_height >= c.vp9_height && c.av1_height > 1080 {
        (Codec::Av1, c.av1_height)
    } else if c.vp9_height > 1080 {
        (Codec::Vp9, c.vp9_height)
    } else {
        (Codec::Avc, 1080)
    };
    // The quality setting offers 480..2160.
    let h = [2160, 1440, 1080].into_iter().find(|&s| s <= h.min(panel.max(1080))).unwrap_or(1080);
    let modern = codec != Codec::Avc;
    (h, codec, c.hdr && modern, modern)
}

/// The media server's decoder entries named `name` (None: not a decoder list).
fn decoders(v: &serde_json::Value, name: &str) -> Option<Vec<serde_json::Value>> {
    let list = v["videoCodecs"].as_array()?;
    Some(list.iter().filter(|c| c["name"].as_str().is_some_and(|n| n.eq_ignore_ascii_case(name))).cloned().collect())
}

/// The decoders' largest heights from the media server's list: `(av1, vp9)`.
pub fn decoder_heights(json: &str) -> (u32, u32) {
    let v: serde_json::Value = serde_json::from_str(json).unwrap_or_default();
    let height = |name: &str| {
        decoders(&v, name)
            .unwrap_or_default()
            .iter()
            .filter(|c| c["maxFrameRate"].as_u64().unwrap_or(0) >= 50)
            .filter_map(|c| c["maxHeight"].as_u64())
            .max()
            .unwrap_or(0) as u32
    };
    (height("AV1"), height("VP9"))
}

/// Whether the media server's list has an `name` decoder at any size or frame rate (None: the
/// text is not a decoder list).
pub fn lists_decoder(json: &str, name: &str) -> Option<bool> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    decoders(&v, name).map(|d| !d.is_empty())
}

/// False only when the TV's decoder list is readable and names no AV1 (the webOS 4 sets: the C9's
/// lists H.264, HEVC and VP9): engine B then never picks an AV1 format. Read once.
#[cfg(not(windows))]
pub fn av1_decoder() -> bool {
    static AV1: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *AV1.get_or_init(|| {
        let listed = std::fs::read_to_string(CODEC_FILE).ok().and_then(|s| lists_decoder(&s, "AV1"));
        if listed == Some(false) {
            crate::log("device: no AV1 decoder in the media server's list; AV1 formats are never chosen");
        }
        listed.unwrap_or(true)
    })
}

#[cfg(not(windows))]
pub fn detect() -> Option<Caps> {
    let (av1_height, vp9_height) = std::fs::read_to_string(CODEC_FILE).map(|s| decoder_heights(&s)).unwrap_or((0, 0));
    let mut c = Caps { panel_height: 0, hdr: false, av1_height, vp9_height };
    // Developer-mode apps are usually refused both services ("Not permitted"): then the decoders
    // decide (below).
    let mut hdr = None;
    match crate::ls2::Luna::shared(plx_base::paths::app_id()) {
        Ok(luna) => {
            let r = luna.call("luna://com.webos.service.config/getConfigs", r#"{"configNames":["tv.model.supportHDR","tv.hw.panelResolution","tv.hw.bSupport_8K_resolution"]}"#);
            let v: serde_json::Value = serde_json::from_str(&r).unwrap_or_default();
            let cfg = &v["configs"];
            hdr = cfg["tv.model.supportHDR"].as_bool();
            c.panel_height = match (cfg["tv.hw.bSupport_8K_resolution"].as_bool(), cfg["tv.hw.panelResolution"].as_str()) {
                (Some(true), _) => 4320,
                (_, Some("UD" | "UHD")) => 2160,
                (_, Some(_)) => 1080,
                _ => 0,
            };
            if c.panel_height == 0 {
                let r = luna.call("luna://com.webos.service.tv.systemproperty/getSystemInfo", r#"{"keys":["UHD"]}"#);
                let v: serde_json::Value = serde_json::from_str(&r).unwrap_or_default();
                c.panel_height = match v["UHD"].as_str() {
                    Some("true") => 2160,
                    Some(_) => 1080,
                    None => 0,
                };
            }
        }
        Err(e) => crate::log(&format!("device: no bus: {e}")),
    }
    if c.panel_height == 0 {
        // No answer: a set whose decoders reach 4K has a 4K panel.
        c.panel_height = if c.av1_height.max(c.vp9_height) >= 2160 { 2160 } else { 1080 };
    }
    // No answer: LG's 4K webOS sets all show HDR10.
    c.hdr = hdr.unwrap_or(c.panel_height >= 2160);
    crate::log(&format!("device: {c:?}"));
    Some(c)
}

#[cfg(windows)]
pub fn detect() -> Option<Caps> {
    None
}

/// First start: raise the playback settings still at their old defaults to what the TV supports.
pub fn apply_once(s: &mut Settings) {
    if s.device_checked {
        return;
    }
    let Some(c) = detect() else { return };
    let (h, codec, hdr, engine_b) = best(&c);
    if s.max_height == 1080 {
        s.max_height = h;
    }
    if s.codec == Codec::Avc {
        s.codec = codec;
    }
    if !s.hdr {
        s.hdr = hdr;
    }
    if engine_b && s.engine == "system" {
        s.engine = "auto".into();
    }
    s.device_checked = true;
    crate::log(&format!("device: quality {}p, {:?}, HDR {}, engine {}", s.max_height, s.codec, s.hdr, s.engine));
    s.save();
}

#[cfg(test)]
mod tests {
    use super::*;

    const LG_C3: &str = r#"{"videoCodecs":[
        {"name":"H.264","maxWidth":4096,"maxHeight":2304,"maxFrameRate":60},
        {"name":"VP8","maxWidth":1920,"maxHeight":1088,"maxFrameRate":30},
        {"name":"VP9","maxWidth":4096,"maxHeight":2304,"maxFrameRate":60},
        {"name":"AV1","maxWidth":4096,"maxHeight":2304,"maxFrameRate":60}]}"#;

    #[test]
    fn oled_c3_gets_4k_av1_hdr() {
        let (av1_height, vp9_height) = decoder_heights(LG_C3);
        assert_eq!((av1_height, vp9_height), (2304, 2304));
        let c = Caps { panel_height: 2160, hdr: true, av1_height, vp9_height };
        assert_eq!(best(&c), (2160, Codec::Av1, true, true));
    }

    #[test]
    fn full_hd_set_without_modern_decoders_stays_h264() {
        let c = Caps { panel_height: 1080, hdr: false, av1_height: 0, vp9_height: 1088 };
        assert_eq!(best(&c), (1080, Codec::Avc, false, false));
    }

    #[test]
    fn av1_decoder_listed() {
        assert_eq!(lists_decoder(LG_C3, "AV1"), Some(true));
        let c9 = r#"{"videoCodecs":[{"name":"H.264","maxHeight":2304,"maxFrameRate":60},{"name":"VP9","maxHeight":2304,"maxFrameRate":60}]}"#;
        assert_eq!(lists_decoder(c9, "AV1"), Some(false));
        assert_eq!(lists_decoder("not json", "AV1"), None);
    }

    #[test]
    fn vp9_only_4k_set() {
        let c = Caps { panel_height: 2160, hdr: true, av1_height: 0, vp9_height: 2304 };
        assert_eq!(best(&c), (2160, Codec::Vp9, true, true));
    }
}
