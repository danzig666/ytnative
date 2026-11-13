//! Minimal InnerTube client: just enough to get stream URLs and prove an authenticated call works.

use crate::bootstrap::{TvConfig, TV_UA};
use crate::clients::ClientProfile;
use anyhow::{bail, Result};
use serde_json::{json, Value};

static CONTENT_COUNTRY: std::sync::RwLock<String> = std::sync::RwLock::new(String::new());

/// Region of the content YouTube returns (InnerTube `gl`, "HU", "DE"...); "" = the default.
pub fn set_content_country(gl: &str) {
    *CONTENT_COUNTRY.write().unwrap() = gl.trim().to_ascii_uppercase();
}

/// The content region set with [`set_content_country`], if any.
pub fn content_country() -> Option<String> {
    let g = CONTENT_COUNTRY.read().unwrap();
    (!g.is_empty()).then(|| g.clone())
}

/// Client profiles. VISIONOS returns direct (un-ciphered, no "n" challenge, no PO token) HTTPS
/// URLs, so no JavaScript engine is needed. TVHTML5 is the logged-in TV client.
/// `Profile` calls with a data-driven profile from `crate::clients` (what `streams::resolve` uses).
#[derive(Clone, Copy)]
pub enum Client {
    VisionOs,
    Tv,
    /// Fallbacks for videos VISIONOS refuses; both return direct URLs anonymously (2026-11).
    AndroidVr,
    Ios,
    Profile(&'static ClientProfile),
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Client::VisionOs => f.write_str("VisionOs"),
            Client::Tv => f.write_str("Tv"),
            Client::AndroidVr => f.write_str("AndroidVr"),
            Client::Ios => f.write_str("Ios"),
            Client::Profile(p) => write!(f, "Profile({})", p.key),
        }
    }
}

const ANDROID_VR_UA: &str = "com.google.android.apps.youtube.vr.oculus/1.65.10 (Linux; U; Android 12L; eureka-user Build/SQ3A.220605.009.A1) gzip";
const IOS_UA: &str = "com.google.ios.youtube/21.26.4 (iPhone16,2; U; CPU iOS 18_3_2 like Mac OS X;)";
const VISIONOS_UA: &str = "Mozilla/5.0 (Macintosh; Intel Mac OS X 15_7_3) AppleWebKit/605.1.15 (KHTML, like Gecko) Version/26.0 Safari/605.1.15";

impl Client {
    pub fn user_agent(self) -> &'static str {
        match self {
            Client::VisionOs => VISIONOS_UA,
            Client::Tv => TV_UA,
            Client::AndroidVr => ANDROID_VR_UA,
            Client::Ios => IOS_UA,
            Client::Profile(p) => &p.user_agent,
        }
    }

    /// InnerTube `context.client` JSON, `X-YouTube-Client-Name` id and client version.
    pub fn context(self, cfg: &TvConfig) -> (Value, String, String) {
        let visitor = cfg.visitor_data.clone().unwrap_or_default();
        let (ctx, name_id, version) = match self {
            Client::Profile(p) => return p.context(cfg),
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
            Client::AndroidVr => (
                json!({
                    "clientName": "ANDROID_VR", "clientVersion": "1.65.10",
                    "deviceMake": "Oculus", "deviceModel": "Quest 3", "androidSdkVersion": 32,
                    "userAgent": ANDROID_VR_UA, "osName": "Android", "osVersion": "12L",
                    "hl": "en", "gl": "US", "visitorData": visitor,
                }),
                "28",
                "1.65.10".into(),
            ),
            Client::Ios => (
                json!({
                    "clientName": "IOS", "clientVersion": "21.26.4",
                    "deviceMake": "Apple", "deviceModel": "iPhone16,2",
                    "userAgent": IOS_UA, "osName": "iPhone", "osVersion": "18.3.2.22D82",
                    "hl": "en", "gl": "US", "visitorData": visitor,
                }),
                "5",
                "21.26.4".into(),
            ),
        };
        (ctx, name_id.to_string(), version)
    }
}

pub struct Api<'a> {
    pub http: &'a reqwest::blocking::Client,
    pub cfg: &'a TvConfig,
}

impl Api<'_> {
    pub fn call(&self, client: Client, endpoint: &str, body: Value, bearer: Option<&str>) -> Result<Value> {
        self.call_with(client, endpoint, body, bearer, &[])
    }

    /// `call` with extra request headers (e.g. `X-Goog-PageId` for a brand account).
    pub fn call_with(
        &self,
        client: Client,
        endpoint: &str,
        mut body: Value,
        bearer: Option<&str>,
        headers: &[(&str, &str)],
    ) -> Result<Value> {
        let (ctx, name_id, version) = client.context(self.cfg);
        body["context"] = json!({ "client": ctx });
        if let Some(gl) = content_country() {
            body["context"]["client"]["gl"] = json!(gl);
        }
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
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let resp = req.send()?;
        let status = resp.status();
        if !status.is_success() {
            let v: Value = resp.json().unwrap_or(Value::Null);
            bail!("{endpoint} -> HTTP {status}: {}", v["error"]["message"]);
        }
        Ok(resp.json()?)
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
