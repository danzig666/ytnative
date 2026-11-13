//! InnerTube client profiles as data.
//!
//! Which player client YouTube serves direct URLs to changes every few months. The profiles
//! (name, version, user agent, device fields) and the order media resolution tries them in (the
//! *ring*) therefore live in `config/clients.json` rather than in code:
//!
//! * the repo copy is compiled in as the built-in default ([`builtin`]);
//! * at run time [`active`] prefers a fresher copy fetched from the repository's `main` branch
//!   ([`REMOTE_URL`]), cached in `paths::data_file("clients.json")` for [`CACHE_TTL`];
//! * anything that fails to download, parse or [validate](Profiles::validate) falls back to a
//!   stale cache and then to the built-ins, so a bad remote file can never break playback.
//!
//! `YTNATIVE_CLIENTS=<path>` loads a local file instead (to try a change before publishing it);
//! `YTNATIVE_CLIENTS=builtin` disables the remote copy.

use crate::bootstrap::TvConfig;
use anyhow::{bail, ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::collections::HashSet;
use std::path::Path;
use std::sync::OnceLock;
use std::time::{Duration, SystemTime};

/// Where an updated profile set is published.
pub const REMOTE_URL: &str = "https://raw.githubusercontent.com/danzig666/ytnative/main/config/clients.json";
/// How long a downloaded copy is used before it is fetched again.
pub const CACHE_TTL: Duration = Duration::from_secs(24 * 60 * 60);
/// Cache file name under `paths::data_file`.
pub const CACHE_FILE: &str = "clients.json";
/// The only schema version this build understands.
pub const SCHEMA: u32 = 1;
/// `clientVersion` placeholder: use the TVHTML5 version scraped by `bootstrap`.
pub const AUTO_VERSION: &str = "auto";

const BUILTIN_JSON: &str = include_str!("../../../config/clients.json");
const FETCH_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Purpose {
    /// Used for `/player` (stream URLs); may appear in the ring.
    Media,
    /// Used for browse/search/actions; never in the ring.
    Metadata,
}

/// One InnerTube client identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ClientProfile {
    /// Stable id used by the ring and in logs ("visionos").
    pub key: String,
    pub purpose: Purpose,
    pub client_name: String,
    /// Literal version, or [`AUTO_VERSION`] for the scraped TVHTML5 version.
    pub client_version: String,
    /// `X-YouTube-Client-Name` header value.
    pub client_name_id: u32,
    pub user_agent: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_make: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub device_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub os_version: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub android_sdk_version: Option<u32>,
    /// Send the signed-in user's bearer token with this client.
    #[serde(default)]
    pub send_auth: bool,
}

impl ClientProfile {
    /// The version actually sent (resolves [`AUTO_VERSION`]).
    pub fn version(&self, cfg: &TvConfig) -> String {
        if self.client_version == AUTO_VERSION {
            cfg.tv_client_version.clone()
        } else {
            self.client_version.clone()
        }
    }

    /// InnerTube `context.client` JSON, `X-YouTube-Client-Name` id and client version.
    pub fn context(&self, cfg: &TvConfig) -> (Value, String, String) {
        let version = self.version(cfg);
        let mut c = json!({
            "clientName": self.client_name,
            "clientVersion": version,
            "userAgent": self.user_agent,
            "hl": "en", "gl": "US",
            "visitorData": cfg.visitor_data.clone().unwrap_or_default(),
        });
        let opt = [
            ("deviceMake", &self.device_make),
            ("deviceModel", &self.device_model),
            ("osName", &self.os_name),
            ("osVersion", &self.os_version),
        ];
        for (k, v) in opt {
            if let Some(v) = v {
                c[k] = json!(v);
            }
        }
        if let Some(sdk) = self.android_sdk_version {
            c["androidSdkVersion"] = json!(sdk);
        }
        (c, self.client_name_id.to_string(), version)
    }

    fn validate(&self) -> Result<()> {
        let k = &self.key;
        ensure!(!k.is_empty() && k.chars().all(|c| c.is_ascii_alphanumeric() || c == '_'), "bad profile key {k:?}");
        ensure!(
            !self.client_name.is_empty() && self.client_name.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == '_'),
            "{k}: bad clientName {:?}",
            self.client_name
        );
        ensure!(
            self.client_version == AUTO_VERSION
                || (!self.client_version.is_empty() && self.client_version.chars().all(|c| c.is_ascii_digit() || c == '.')),
            "{k}: bad clientVersion {:?}",
            self.client_version
        );
        ensure!(self.client_name_id > 0, "{k}: clientNameId must be > 0");
        // Everything below ends up in an HTTP header or the request body: printable ASCII only.
        let fields = [
            Some(&self.user_agent),
            self.device_make.as_ref(),
            self.device_model.as_ref(),
            self.os_name.as_ref(),
            self.os_version.as_ref(),
        ];
        for f in fields.into_iter().flatten() {
            ensure!(f.len() <= 512 && f.chars().all(|c| (' '..='~').contains(&c)), "{k}: non-printable or oversized field");
        }
        ensure!(!self.user_agent.is_empty(), "{k}: empty userAgent");
        Ok(())
    }
}

/// A validated profile set: every profile plus the media resolution order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Profiles {
    pub schema: u32,
    /// Profile keys tried in order by `streams::resolve`.
    pub ring: Vec<String>,
    pub profiles: Vec<ClientProfile>,
}

/// Where the active profile set came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Source {
    Builtin,
    /// Fresh cache file.
    Cache,
    /// Just downloaded from [`REMOTE_URL`].
    Remote,
    /// Cache older than the TTL, used because the download failed.
    StaleCache,
    /// `YTNATIVE_CLIENTS=<path>`.
    Override(String),
}

impl Profiles {
    /// Parses and validates a profile file.
    pub fn parse(text: &str) -> Result<Profiles> {
        let p: Profiles = serde_json::from_str(text).context("parsing client profiles")?;
        p.validate()?;
        Ok(p)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == SCHEMA, "unsupported clients schema {} (want {SCHEMA})", self.schema);
        ensure!(!self.profiles.is_empty() && self.profiles.len() <= 64, "profile count out of range");
        let mut keys = HashSet::new();
        for p in &self.profiles {
            p.validate()?;
            ensure!(keys.insert(p.key.as_str()), "duplicate profile key {:?}", p.key);
        }
        ensure!(!self.ring.is_empty(), "empty ring");
        let mut seen = HashSet::new();
        for k in &self.ring {
            let Some(p) = self.get(k) else { bail!("ring names unknown profile {k:?}") };
            ensure!(p.purpose == Purpose::Media, "ring entry {k:?} is not a media profile");
            ensure!(seen.insert(k.as_str()), "ring lists {k:?} twice");
        }
        Ok(())
    }

    pub fn get(&self, key: &str) -> Option<&ClientProfile> {
        self.profiles.iter().find(|p| p.key == key)
    }

    /// The ring as profiles, in order.
    pub fn ring_profiles(&self) -> impl Iterator<Item = &ClientProfile> {
        self.ring.iter().filter_map(|k| self.get(k))
    }
}

/// The compiled-in defaults (`config/clients.json` at build time).
pub fn builtin() -> &'static Profiles {
    static B: OnceLock<Profiles> = OnceLock::new();
    B.get_or_init(|| Profiles::parse(BUILTIN_JSON).expect("built-in config/clients.json is invalid"))
}

static ACTIVE: OnceLock<(Profiles, Source)> = OnceLock::new();

/// The profile set in effect for this process, loaded on first use (see module docs).
pub fn active(http: &reqwest::blocking::Client) -> &'static Profiles {
    &ACTIVE.get_or_init(|| load(http)).0
}

/// Where [`active`] got its profiles from, once loaded.
pub fn active_source() -> Option<&'static Source> {
    ACTIVE.get().map(|a| &a.1)
}

/// Installs a profile set before first use (e.g. loaded on a background thread at boot).
/// Returns false if one is already active.
pub fn set_active(p: Profiles, source: Source) -> bool {
    ACTIVE.set((p, source)).is_ok()
}

/// Loads the profile set: env override, fresh cache, download, stale cache, built-ins.
pub fn load(http: &reqwest::blocking::Client) -> (Profiles, Source) {
    match std::env::var("YTNATIVE_CLIENTS").ok().as_deref() {
        Some("builtin") => return (builtin().clone(), Source::Builtin),
        Some(path) if !path.is_empty() => {
            match std::fs::read_to_string(path).map_err(anyhow::Error::from).and_then(|t| Profiles::parse(&t)) {
                Ok(p) => return (p, Source::Override(path.to_string())),
                Err(e) => eprintln!("[clients] YTNATIVE_CLIENTS={path}: {e:#}; using built-ins"),
            }
            return (builtin().clone(), Source::Builtin);
        }
        _ => {}
    }
    load_with(&crate::paths::data_file(CACHE_FILE), SystemTime::now(), || fetch(http))
}

fn fetch(http: &reqwest::blocking::Client) -> Result<String> {
    Ok(http.get(REMOTE_URL).timeout(FETCH_TIMEOUT).send()?.error_for_status()?.text()?)
}

/// [`load`] without the environment and network, for tests: `fetch` is only called when the
/// cache is missing, stale or invalid.
pub fn load_with(cache: &Path, now: SystemTime, fetch: impl FnOnce() -> Result<String>) -> (Profiles, Source) {
    let cached = std::fs::read_to_string(cache).ok().and_then(|t| Profiles::parse(&t).ok());
    let fresh = std::fs::metadata(cache)
        .and_then(|m| m.modified())
        .ok()
        // A modification time in the future (clock moved back) counts as fresh.
        .is_some_and(|m| now.duration_since(m).map_or(true, |age| age < CACHE_TTL));
    if let (Some(p), true) = (&cached, fresh) {
        return (p.clone(), Source::Cache);
    }
    match fetch().and_then(|t| Profiles::parse(&t).map(|p| (p, t))) {
        Ok((p, text)) => {
            let _ = std::fs::write(cache, text);
            (p, Source::Remote)
        }
        Err(e) => {
            eprintln!("[clients] remote profiles unavailable: {e:#}");
            match cached {
                Some(p) => {
                    // Mark the attempt: the next start uses the cache without a 5 s fetch stall.
                    if let Ok(f) = std::fs::File::options().append(true).open(cache) {
                        let _ = f.set_modified(now);
                    }
                    (p, Source::StaleCache)
                }
                None => (builtin().clone(), Source::Builtin),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::innertube::Client;

    fn cfg() -> TvConfig {
        TvConfig {
            client_id: String::new(),
            client_secret: String::new(),
            tv_client_version: "7.20261110.12.00".into(),
            visitor_data: Some("VISITOR".into()),
            scraped: false,
        }
    }

    fn tmp(name: &str) -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("ytcore-clients-{}-{name}", std::process::id()));
        let _ = std::fs::remove_file(&d);
        d
    }

    #[test]
    fn builtin_parses_and_matches_hardcoded_clients() {
        let b = builtin();
        assert_eq!(b.ring, ["visionos", "android_vr", "ios"]);
        let cfg = cfg();
        for (key, client) in [("visionos", Client::VisionOs), ("android_vr", Client::AndroidVr), ("ios", Client::Ios), ("tv", Client::Tv)] {
            let p = b.get(key).unwrap();
            assert_eq!(p.context(&cfg), client.context(&cfg), "{key}");
            assert_eq!(p.user_agent, client.user_agent(), "{key}");
            let pc = Client::Profile(p);
            assert_eq!(pc.context(&cfg), client.context(&cfg));
        }
        // Today's ring sends no auth.
        assert!(b.ring_profiles().all(|p| !p.send_auth));
    }

    #[test]
    fn roundtrip_serialization() {
        let s = serde_json::to_string(builtin()).unwrap();
        assert_eq!(&Profiles::parse(&s).unwrap(), builtin());
    }

    #[test]
    fn validation_rejects_bad_files() {
        let good: Value = serde_json::from_str(BUILTIN_JSON).unwrap();
        let bad = |f: &dyn Fn(&mut Value)| {
            let mut v = good.clone();
            f(&mut v);
            Profiles::parse(&v.to_string()).is_err()
        };
        assert!(bad(&|v| v["schema"] = json!(2)));
        assert!(bad(&|v| v["ring"] = json!([])));
        assert!(bad(&|v| v["ring"] = json!(["nope"])));
        assert!(bad(&|v| v["ring"] = json!(["tv"])), "metadata profile in ring");
        assert!(bad(&|v| v["ring"] = json!(["ios", "ios"])));
        assert!(bad(&|v| v["profiles"][1]["key"] = json!("visionos")), "duplicate key");
        assert!(bad(&|v| v["profiles"][0]["userAgent"] = json!("a\r\nX-Evil: 1")));
        assert!(bad(&|v| v["profiles"][0]["clientVersion"] = json!("")));
        assert!(bad(&|v| v["profiles"][0]["clientName"] = json!("visionos lower")));
        assert!(bad(&|v| v["profiles"][0]["clientNameId"] = json!(0)));
        assert!(bad(&|v| v["profiles"][0]["purpose"] = json!("other")));
        assert!(bad(&|v| v["profiles"][0]["surprise"] = json!(1)), "unknown field");
        assert!(Profiles::parse("not json").is_err());
        assert!(!bad(&|v| v["ring"] = json!(["ios", "visionos"])));
    }

    #[test]
    fn fallback_chain() {
        let now = SystemTime::now();
        // No cache, download fails -> built-ins, nothing written.
        let c = tmp("a");
        let (p, s) = load_with(&c, now, || bail!("offline"));
        assert_eq!((&p, s), (builtin(), Source::Builtin));
        assert!(!c.exists());

        // No cache, download returns garbage -> built-ins.
        let (_, s) = load_with(&c, now, || Ok("{\"schema\":1}".into()));
        assert_eq!(s, Source::Builtin);
        assert!(!c.exists());

        // Valid download (reordered ring) -> used and cached.
        let mut v: Value = serde_json::from_str(BUILTIN_JSON).unwrap();
        v["ring"] = json!(["ios", "visionos"]);
        let remote = v.to_string();
        let (p, s) = load_with(&c, now, || Ok(remote.clone()));
        assert_eq!((p.ring.as_slice(), s), (&["ios".to_string(), "visionos".to_string()][..], Source::Remote));
        assert!(c.exists());

        // Fresh cache -> no fetch.
        let (p, s) = load_with(&c, now, || panic!("must not fetch"));
        assert_eq!((p.ring[0].as_str(), s), ("ios", Source::Cache));

        // Stale cache + failed download -> stale cache.
        let later = now + CACHE_TTL + Duration::from_secs(60);
        let (p, s) = load_with(&c, later, || bail!("offline"));
        assert_eq!((p.ring[0].as_str(), s), ("ios", Source::StaleCache));

        // Corrupt cache + failed download -> built-ins.
        std::fs::write(&c, "{ broken").unwrap();
        let (p, s) = load_with(&c, now, || bail!("offline"));
        assert_eq!((&p, s), (builtin(), Source::Builtin));
        let _ = std::fs::remove_file(&c);
    }

    /// Resolves a public video by walking the profile ring (network).
    #[test]
    #[ignore]
    fn live_resolve_via_profiles() {
        let http = reqwest::blocking::Client::builder().gzip(true).build().unwrap();
        let cfg = crate::bootstrap::fetch(&http).unwrap();
        let api = crate::innertube::Api { http: &http, cfg: &cfg };
        let r = crate::streams::resolve(&api, "dQw4w9WgXcQ", None).unwrap();
        eprintln!("resolved via {:?} from {:?}", r.client, active_source());
        assert!(matches!(r.client, Client::Profile(_)));
        assert!(r.adaptive.iter().any(|f| f["url"].is_string()));
        // Every built-in media profile can also be called directly.
        for p in builtin().ring_profiles() {
            let pr = api.player(Client::Profile(p), "dQw4w9WgXcQ", None).unwrap();
            eprintln!("{}: {}", p.key, pr["playabilityStatus"]["status"]);
        }
    }
}
