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

/// Clients tried in order. Each entry: (client, send auth?).
pub const RING: &[(Client, bool)] = &[(Client::VisionOs, false)];

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

#[derive(Debug, Clone, Copy)]
pub struct Prefs {
    pub min_height: u64,
    pub max_height: u64,
    /// Video codec family for the DASH manifest: "avc1" today (engine A), "vp09"/"av01" with the
    /// Feed engine later.
    pub codec: &'static str,
}

impl Default for Prefs {
    fn default() -> Self {
        Prefs { min_height: 720, max_height: 1080, codec: "avc1" }
    }
}

pub fn resolve(api: &Api, video_id: &str, bearer: Option<&str>) -> Result<Resolved> {
    let mut errors = Vec::new();
    for &(client, auth) in RING {
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
                if direct || r.hls.is_some() || r.dash_live.is_some() {
                    return Ok(r);
                }
                errors.push(format!("{client:?}: no direct formats"));
            }
            Err(e) => errors.push(format!("{client:?}: {e}")),
        }
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
        mpd::build(&self.adaptive, dur, prefs.codec, prefs.min_height, prefs.max_height).map(|m| m.0)
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
