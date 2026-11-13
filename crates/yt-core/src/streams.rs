//! Stream resolution: try player clients in order until one yields playable, directly fetchable
//! formats, and collect everything the player screen needs (captions, storyboard, live state).
//!
//! The ring is data so a YouTube change can be answered by reordering, not by a rewrite.
//! Today (2026-11) VISIONOS returns direct URLs with no cipher, no `n` challenge and no PO token;
//! the signed-in TV client returns SABR only (kept for when the SABR engine lands).

use crate::innertube::{text, Api, Client};
use crate::mpd;
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;

// The clients tried, their order and whether they get the bearer token come from
// `clients::active()` (built-in `config/clients.json`, optionally refreshed from the repo).

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaptionTrack {
    pub base_url: String,
    pub name: String,
    pub language: String,
    /// Auto-generated (ASR) track.
    pub auto: bool,
    pub translatable: bool,
}

#[derive(Debug, Clone)]
pub struct Resolved {
    pub client: Client,
    pub video_id: String,
    pub title: String,
    pub author: String,
    pub channel_id: String,
    pub length_secs: u64,
    pub is_live: bool,
    pub is_upcoming: bool,
    pub adaptive: Vec<Value>,
    pub hls: Option<String>,
    pub dash_live: Option<String>,
    pub sabr: Option<String>,
    pub expires_secs: u64,
    pub captions: Vec<CaptionTrack>,
    /// (language code, name) pairs captions can be auto-translated into.
    pub translations: Vec<(String, String)>,
    pub storyboard_spec: Option<String>,
    pub raw: Value,
}

#[derive(Debug, Clone)]
pub struct Prefs {
    pub min_height: u64,
    pub max_height: u64,
    /// Video codec family for the DASH manifest: "avc1" today (engine A), "vp09"/"av01" with the
    /// Feed engine later.
    pub codec: &'static str,
    /// Audio track id for multi-language videos (None = the default track).
    pub audio_track: Option<String>,
    /// Preferred audio language ("de", "pt-BR"): the track in it when `audio_track` is not set
    /// (None = the default track).
    pub audio_language: Option<String>,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { min_height: 720, max_height: 1080, codec: "avc1", audio_track: None, audio_language: None }
    }
}

pub fn resolve(api: &Api, video_id: &str, bearer: Option<&str>) -> Result<Resolved> {
    let mut errors = Vec::new();
    // Direct URLs, but nothing the TV's DASH player takes (no H.264 + AAC with ranges): kept in
    // case no later client does better (engine B still plays VP9/AV1).
    let mut partial: Option<Resolved> = None;
    for profile in crate::clients::active(api.http).ring_profiles() {
        let (client, auth) = (Client::Profile(profile), profile.send_auth);
        match api.player(client, video_id, if auth { bearer } else { None }) {
            Ok(pr) => {
                let status = pr["playabilityStatus"]["status"].as_str().unwrap_or("?").to_string();
                if status != "OK" {
                    let reason = pr["playabilityStatus"]["reason"].as_str().unwrap_or("").to_string();
                    errors.push(format!("{client:?}: {status} {reason}"));
                    continue;
                }
                let r = build(client, video_id, pr);
                let direct = r.adaptive.iter().any(|f| f["url"].is_string());
                if r.hls.is_some() || r.dash_live.is_some() || (direct && r.mpd(Default::default()).is_some()) {
                    return Ok(r);
                }
                if direct {
                    errors.push(format!("{client:?}: no H.264 + AAC"));
                    partial.get_or_insert(r);
                } else {
                    errors.push(format!("{client:?}: no direct formats"));
                }
            }
            Err(e) => errors.push(format!("{client:?}: {e}")),
        }
    }
    if let Some(r) = partial {
        return Ok(r);
    }
    bail!("no client could play {video_id}: {}", errors.join("; "))
}

fn build(client: Client, video_id: &str, pr: Value) -> Resolved {
    let d = &pr["videoDetails"];
    let sd = &pr["streamingData"];
    let caps = &pr["captions"]["playerCaptionsTracklistRenderer"];
    let captions = caps["captionTracks"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|t| {
                    Some(CaptionTrack {
                        base_url: t["baseUrl"].as_str()?.to_string(),
                        name: text(&t["name"]).unwrap_or_default(),
                        language: t["languageCode"].as_str().unwrap_or("").to_string(),
                        auto: t["kind"].as_str() == Some("asr"),
                        translatable: t["isTranslatable"].as_bool().unwrap_or(false),
                    })
                })
                .collect()
        })
        .unwrap_or_default();
    let translations = caps["translationLanguages"]
        .as_array()
        .map(|a| {
            a.iter()
                .filter_map(|l| Some((l["languageCode"].as_str()?.to_string(), text(&l["languageName"]).unwrap_or_default())))
                .collect()
        })
        .unwrap_or_default();
    Resolved {
        client,
        video_id: video_id.to_string(),
        title: d["title"].as_str().unwrap_or("").to_string(),
        author: d["author"].as_str().unwrap_or("").to_string(),
        channel_id: d["channelId"].as_str().unwrap_or("").to_string(),
        length_secs: d["lengthSeconds"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
        is_live: d["isLive"].as_bool().unwrap_or(false),
        is_upcoming: d["isUpcoming"].as_bool().unwrap_or(false),
        adaptive: sd["adaptiveFormats"].as_array().cloned().unwrap_or_default(),
        hls: sd["hlsManifestUrl"].as_str().map(str::to_string),
        dash_live: sd["dashManifestUrl"].as_str().map(str::to_string),
        sabr: sd["serverAbrStreamingUrl"].as_str().map(str::to_string),
        expires_secs: sd["expiresInSeconds"].as_str().and_then(|s| s.parse().ok()).unwrap_or(0),
        captions,
        translations,
        storyboard_spec: pr["storyboards"]["playerStoryboardSpecRenderer"]["spec"].as_str().map(str::to_string),
        raw: pr,
    }
}

impl Resolved {
    /// DASH manifest for engine A (URI + firmware dashdemux2).
    pub fn mpd(&self, prefs: Prefs) -> Option<String> {
        let dur = self
            .adaptive
            .iter()
            .find_map(|f| f["approxDurationMs"].as_str()?.parse::<u64>().ok())
            .unwrap_or(self.length_secs * 1000);
        let track = prefs.audio_track.clone().or_else(|| prefs.audio_language.as_deref().and_then(|l| self.audio_track_for(l)));
        mpd::build_with_audio(&self.adaptive, dur, prefs.codec, prefs.min_height, prefs.max_height, track.as_deref()).map(|m| m.0)
    }

    /// The audio track id in language `lang` ("de" matches "de" and "de-DE"; ids look like
    /// "de-DE.3"), if the video offers one.
    pub fn audio_track_for(&self, lang: &str) -> Option<String> {
        let base = |l: &str| l.split(['-', '_']).next().unwrap_or(l).to_ascii_lowercase();
        let want = base(lang);
        let tracks = self.audio_tracks();
        let lang_of = |id: &str| id.split('.').next().unwrap_or(id).to_string();
        tracks
            .iter()
            .find(|t| lang_of(&t.0).eq_ignore_ascii_case(lang))
            .or_else(|| tracks.iter().find(|t| base(&lang_of(&t.0)) == want))
            .map(|t| t.0.clone())
    }

    /// Audio languages offered: (track id, display name, is default), deduplicated.
    pub fn audio_tracks(&self) -> Vec<(String, String, bool)> {
        let mut out: Vec<(String, String, bool)> = Vec::new();
        for f in &self.adaptive {
            let t = &f["audioTrack"];
            if let Some(id) = t["id"].as_str() {
                if !out.iter().any(|o| o.0 == id) {
                    out.push((id.to_string(), t["displayName"].as_str().unwrap_or(id).to_string(), t["audioIsDefault"].as_bool().unwrap_or(false)));
                }
            }
        }
        out
    }

    /// Live / HLS: the master playlist reduced to H.264 renditions up to `max_height`, keeping the
    /// audio groups they use. Returned as text to be written to a local file for the player.
    pub fn hls_master(&self, http: &reqwest::blocking::Client, max_height: u64) -> anyhow::Result<String> {
        let url = self.hls.as_deref().ok_or_else(|| anyhow::anyhow!("no HLS manifest"))?;
        let text = http.get(url).send()?.error_for_status()?.text()?;
        Ok(filter_hls(&text, max_height))
    }

    /// Caption download URL in the given format ("vtt" or "ttml"), optionally auto-translated.
    pub fn caption_url(&self, track: &CaptionTrack, fmt: &str, translate_to: Option<&str>) -> String {
        let mut u = format!("{}&fmt={fmt}", track.base_url);
        if let Some(l) = translate_to {
            u.push_str(&format!("&tlang={l}"));
        }
        u
    }
}

/// Keeps `#EXT-X-STREAM-INF` variants whose CODECS start with avc1 and whose height is within
/// `max_height`, plus the `#EXT-X-MEDIA` groups they reference and all global tags.
pub fn filter_hls(master: &str, max_height: u64) -> String {
    let lines: Vec<&str> = master.lines().collect();
    let mut keep_groups = std::collections::HashSet::new();
    let mut variants = Vec::new();
    let mut i = 0;
    while i < lines.len() {
        let l = lines[i];
        if l.starts_with("#EXT-X-STREAM-INF") {
            let uri = lines.get(i + 1).copied().unwrap_or("");
            let height = l
                .split("RESOLUTION=")
                .nth(1)
                .and_then(|r| r.split(',').next())
                .and_then(|r| r.split('x').nth(1))
                .and_then(|h| h.parse::<u64>().ok())
                .unwrap_or(0);
            if l.contains("avc1") && height <= max_height {
                if let Some(g) = l.split("AUDIO=\"").nth(1).and_then(|r| r.split('"').next()) {
                    keep_groups.insert(g.to_string());
                }
                variants.push((l, uri));
            }
            i += 2;
            continue;
        }
        i += 1;
    }
    let mut out = Vec::new();
    for l in &lines {
        if l.starts_with("#EXT-X-STREAM-INF") || (!l.starts_with('#') && !l.trim().is_empty()) {
            continue;
        }
        if l.starts_with("#EXT-X-MEDIA") {
            let g = l.split("GROUP-ID=\"").nth(1).and_then(|r| r.split('"').next()).unwrap_or("");
            if !keep_groups.contains(g) {
                continue;
            }
        }
        out.push(l.to_string());
    }
    if variants.is_empty() {
        // Nothing matched (no CODECS attribute, or only taller rungs): the unfiltered master
        // plays somehow; an empty one never does.
        return master.to_string();
    }
    for (inf, uri) in variants {
        out.push(inf.to_string());
        out.push(uri.to_string());
    }
    out.join("\n") + "\n"
}

#[cfg(test)]
mod hls_tests {
    #[test]
    fn keeps_h264_within_height() {
        let m = "#EXTM3U
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a1\",URI=\"x\"
#EXT-X-MEDIA:TYPE=AUDIO,GROUP-ID=\"a2\",URI=\"y\"
#EXT-X-STREAM-INF:BANDWIDTH=1,CODECS=\"avc1.4d401f,mp4a\",RESOLUTION=1280x720,AUDIO=\"a1\"
v720
#EXT-X-STREAM-INF:BANDWIDTH=2,CODECS=\"avc1.64002a,mp4a\",RESOLUTION=1920x1080,AUDIO=\"a1\"
v1080
#EXT-X-STREAM-INF:BANDWIDTH=3,CODECS=\"vp09.00.50.08,mp4a\",RESOLUTION=2560x1440,AUDIO=\"a2\"
vp9
";
        let f = super::filter_hls(m, 1080);
        assert!(f.contains("v720") && f.contains("v1080") && !f.contains("vp9"));
        assert!(f.contains("GROUP-ID=\"a1\"") && !f.contains("GROUP-ID=\"a2\""));
        let f = super::filter_hls(m, 720);
        assert!(f.contains("v720") && !f.contains("v1080"));
    }
}
