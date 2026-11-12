//! Minimal InnerTube client: just enough to get stream URLs and prove an authenticated call works.

use crate::bootstrap::{TvConfig, TV_UA};
use anyhow::{bail, Result};
use serde_json::{json, Value};

/// Client profiles. VISIONOS returns direct (un-ciphered, no "n" challenge, no PO token) HTTPS
/// URLs, so no JavaScript engine is needed. TVHTML5 is the logged-in TV client.
#[derive(Clone, Copy, Debug)]
pub enum Client {
    VisionOs,
    Tv,
}

const VISIONOS_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

impl Client {
    pub fn user_agent(self) -> &'static str {
        match self {
            Client::VisionOs => VISIONOS_UA,
            Client::Tv => TV_UA,
        }
    }

    /// InnerTube `context.client` JSON, `X-YouTube-Client-Name` id and client version.
    pub fn context(self, cfg: &TvConfig) -> (Value, &'static str, String) {
        let visitor = cfg.visitor_data.clone().unwrap_or_default();
        match self {
            Client::VisionOs => (
                json!({
                    "clientName": "VISIONOS", "clientVersion": "1.02",
                    "deviceMake": "Apple", "deviceModel": "RealityDevice17,1",
                    "userAgent": VISIONOS_UA, "osName": "visionOS", "osVersion": "26.5.23O471",
                    "hl": "en", "gl": "US", "visitorData": visitor,
                }),
                "101",
                "1.02".into(),
            ),
            Client::Tv => (
                json!({
                    "clientName": "TVHTML5", "clientVersion": cfg.tv_client_version,
                    "userAgent": TV_UA, "hl": "en", "gl": "US", "visitorData": visitor,
                }),
                "7",
                cfg.tv_client_version.clone(),
            ),
        }
    }
}

pub struct Api<'a> {
    pub http: &'a reqwest::blocking::Client,
    pub cfg: &'a TvConfig,
}

impl Api<'_> {
    pub fn call(&self, client: Client, endpoint: &str, mut body: Value, bearer: Option<&str>) -> Result<Value> {
        let (ctx, name_id, version) = client.context(self.cfg);
        body["context"] = json!({ "client": ctx });
        let mut req = self
            .http
            .post(format!("https://www.youtube.com/youtubei/v1/{endpoint}?prettyPrint=false"))
            .header("User-Agent", client.user_agent())
            .header("X-YouTube-Client-Name", name_id)
            .header("X-YouTube-Client-Version", version)
            .header("Origin", "https://www.youtube.com")
            .json(&body);
        if let Some(v) = &self.cfg.visitor_data {
            req = req.header("X-Goog-Visitor-Id", v);
        }
        if let Some(t) = bearer {
            req = req.bearer_auth(t);
        }
        let resp = req.send()?;
        let status = resp.status();
        let v: Value = resp.json()?;
        if !status.is_success() {
            bail!("{endpoint} -> HTTP {status}: {}", v["error"]["message"]);
        }
        Ok(v)
    }

    pub fn player(&self, client: Client, video_id: &str, bearer: Option<&str>) -> Result<Value> {
        self.call(
            client,
            "player",
            json!({
                "videoId": video_id,
                "playbackContext": { "contentPlaybackContext": { "html5Preference": "HTML5_PREF_WANTS" } },
                "contentCheckOk": true,
                "racyCheckOk": true,
            }),
            bearer,
        )
    }
}

// ---------- small JSON helpers ----------

/// Text of a YouTube "formatted string" ({simpleText} | {runs:[{text}]} | {content}).
pub fn text(v: &Value) -> Option<String> {
    if let Some(s) = v.as_str() {
        return Some(s.into());
    }
    if let Some(s) = v["simpleText"].as_str().or(v["content"].as_str()) {
        return Some(s.into());
    }
    let runs = v["runs"].as_array()?;
    Some(runs.iter().filter_map(|r| r["text"].as_str()).collect())
}

/// Depth-first search for the first value under `key`.
pub fn find<'a>(v: &'a Value, key: &str) -> Option<&'a Value> {
    match v {
        Value::Object(m) => m.get(key).or_else(|| m.values().find_map(|c| find(c, key))),
        Value::Array(a) => a.iter().find_map(|c| find(c, key)),
        _ => None,
    }
}

/// Collects (videoId, title) pairs from TV-style tiles (tileRenderer / lockupViewModel).
pub fn video_tiles(v: &Value, out: &mut Vec<(String, String)>) {
    match v {
        Value::Object(m) => {
            if let Some(t) = m.get("tileRenderer") {
                if let Some(id) = t["contentId"].as_str().or(find(t, "videoId").and_then(Value::as_str)) {
                    let title = text(&t["metadata"]["tileMetadataRenderer"]["title"]).unwrap_or_default();
                    out.push((id.into(), title));
                }
            } else if let Some(t) = m.get("lockupViewModel") {
                if let Some(id) = t["contentId"].as_str() {
                    let title = text(&t["metadata"]["lockupMetadataViewModel"]["title"]).unwrap_or_default();
                    out.push((id.into(), title));
                }
            }
            m.values().for_each(|c| video_tiles(c, out));
        }
        Value::Array(a) => a.iter().for_each(|c| video_tiles(c, out)),
        _ => {}
    }
}
