//! Updates from GitHub releases: a check reads the latest release's Homebrew Channel manifest,
//! the install hands it to Homebrew Channel's service (`org.webosbrew.hbchannel.service/install`),
//! which downloads the package, checks its SHA-256 and installs it. The app is not allowed to
//! install packages itself, and installing closes it.

use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const MANIFEST: &str = "https://github.com/danzig666/ytnative/releases/latest/download/io.github.danzig666.ytnative.manifest.json";
/// The automatic check runs at a start at most this often.
const AUTO_EVERY_SECS: u64 = 20 * 3600;
pub const NEEDS_HBCHANNEL: &str = "Homebrew Channel is needed to install updates";

#[derive(Clone, Debug, PartialEq)]
pub struct Release {
    pub version: String,
    pub ipk_url: String,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq)]
pub enum State {
    Unknown,
    Checking,
    UpToDate,
    Available(Release),
    CheckFailed(String),
    /// Homebrew Channel is downloading (percent, when it says).
    Downloading(Option<u32>),
    Installing,
    Installed,
    InstallFailed(String),
}

static STATE: Mutex<State> = Mutex::new(State::Unknown);
/// The newer release the last check found (kept through a failed install, to try again).
static FOUND: Mutex<Option<Release>> = Mutex::new(None);

/// A newer release is known: the About page offers to install it.
pub fn found() -> bool {
    FOUND.lock().unwrap().is_some()
}

pub fn found_version() -> Option<String> {
    FOUND.lock().unwrap().as_ref().map(|r| r.version.clone())
}

/// The automatic check found a newer version and nobody has been asked yet.
static PROMPT: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

/// Whether to ask now (once): the caller shows the prompt.
pub fn take_prompt() -> bool {
    PROMPT.swap(false, std::sync::atomic::Ordering::Relaxed)
}

pub fn state() -> State {
    STATE.lock().unwrap().clone()
}

fn set(s: State) {
    *STATE.lock().unwrap() = s;
    plx_machine::present::wake_from_worker();
}

pub fn current_version() -> &'static str {
    const INFO: &str = include_str!("../pkg/appinfo.json");
    INFO.split("\"version\"").nth(1).and_then(|rest| rest.split('"').nth(1)).unwrap_or(env!("CARGO_PKG_VERSION"))
}

/// Dotted version numbers compared numerically ("0.2.10" is newer than "0.2.9").
pub fn newer(candidate: &str, current: &str) -> bool {
    let parse = |v: &str| v.trim_start_matches('v').split('.').map(|p| p.parse::<u64>().unwrap_or(0)).collect::<Vec<_>>();
    let (a, mut b) = (parse(candidate), parse(current));
    b.resize(a.len().max(b.len()), 0);
    let mut a = a;
    a.resize(b.len(), 0);
    a > b
}

/// Reads the release from a Homebrew Channel manifest.
pub fn parse_manifest(json: &str) -> Option<Release> {
    let v: serde_json::Value = serde_json::from_str(json).ok()?;
    Some(Release {
        version: v["version"].as_str()?.to_string(),
        ipk_url: v["ipkUrl"].as_str()?.to_string(),
        sha256: v["ipkHash"]["sha256"].as_str()?.to_string(),
    })
}

fn fetch_latest() -> anyhow::Result<Release> {
    let http = reqwest::blocking::Client::builder().timeout(Duration::from_secs(20)).build()?;
    let body = http.get(MANIFEST).send()?.error_for_status()?.text()?;
    parse_manifest(&body).ok_or_else(|| anyhow::anyhow!("unreadable release information"))
}

/// Checks in the background; the About page shows the result.
pub fn check_now() {
    check(false);
}

fn check(prompt: bool) {
    {
        let mut s = STATE.lock().unwrap();
        if matches!(*s, State::Checking | State::Downloading(_) | State::Installing) {
            return;
        }
        *s = State::Checking;
    }
    plx_machine::present::wake_from_worker();
    let started = plx_base::task::spawn("update-check", move || {
        let r = fetch_latest();
        let _ = std::fs::write(stamp_path(), now_secs().to_string());
        *FOUND.lock().unwrap() = r.as_ref().ok().filter(|rel| newer(&rel.version, current_version())).cloned();
        set(match r {
            Ok(rel) if newer(&rel.version, current_version()) => {
                crate::log(&format!("update: version {} available", rel.version));
                if prompt {
                    PROMPT.store(true, std::sync::atomic::Ordering::Relaxed);
                }
                State::Available(rel)
            }
            Ok(_) => State::UpToDate,
            Err(e) => {
                crate::log(&format!("update: check failed: {e:#}"));
                State::CheckFailed(format!("{e:#}"))
            }
        });
    });
    if started.is_none() {
        set(State::CheckFailed("could not start".into()));
    }
}

fn now_secs() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_secs())
}

fn stamp_path() -> std::path::PathBuf {
    yt_core::paths::data_file("update-checked")
}

/// At start: checks when the last check is older than `AUTO_EVERY_SECS` (in the simulator only
/// with `YTNATIVE_SIM_UPDATE` set).
pub fn start_auto_check() {
    if cfg!(windows) && std::env::var_os("YTNATIVE_SIM_UPDATE").is_none() {
        return;
    }
    let last = std::fs::read_to_string(stamp_path()).ok().and_then(|s| s.trim().parse::<u64>().ok()).unwrap_or(0);
    if now_secs().saturating_sub(last) >= AUTO_EVERY_SECS {
        check(true);
    }
}

/// One reply of Homebrew Channel's install, read into a state (None: nothing to show).
pub fn read_install_reply(reply: &str) -> Option<State> {
    let v: serde_json::Value = serde_json::from_str(reply).ok()?;
    if v["returnValue"] == serde_json::Value::Bool(false) || v.get("errorText").is_some() {
        let t = v["errorText"].as_str().unwrap_or("the installer refused");
        // No Homebrew Channel on this TV: the bus answers for the missing service.
        if t.contains("does not exist") || t.contains("Unknown service") {
            return Some(State::InstallFailed(NEEDS_HBCHANNEL.into()));
        }
        return Some(State::InstallFailed(t.to_string()));
    }
    if v["finished"] == serde_json::Value::Bool(true) {
        return Some(State::Installed);
    }
    match v["statusText"].as_str() {
        Some(t) if t.starts_with("Download") => Some(State::Downloading(v["progress"].as_f64().map(|p| p.clamp(0.0, 100.0) as u32))),
        Some(t) if t.starts_with("Verif") || t.starts_with("Install") => Some(State::Installing),
        _ => None,
    }
}

/// Hands the available release to Homebrew Channel. Installing closes the app.
pub fn install() {
    let Some(rel) = FOUND.lock().unwrap().clone() else { return };
    {
        let mut s = STATE.lock().unwrap();
        if matches!(*s, State::Checking | State::Downloading(_) | State::Installing | State::Installed) {
            return;
        }
        *s = State::Downloading(None);
    }
    plx_machine::present::wake_from_worker();
    crate::log(&format!("update: installing {} through Homebrew Channel", rel.version));
    #[cfg(not(windows))]
    {
        let payload = serde_json::json!({"ipkUrl": rel.ipk_url, "ipkHash": rel.sha256, "id": plx_base::paths::app_id(), "subscribe": true}).to_string();
        let sent = match crate::ls2::Luna::shared(plx_base::paths::app_id()) {
            Ok(luna) => luna.subscribe("luna://org.webosbrew.hbchannel.service/install", &payload, |reply| {
                if let Some(s) = read_install_reply(reply) {
                    if let State::InstallFailed(e) = &s {
                        crate::log(&format!("update: install failed: {e}"));
                    }
                    // A late reply never turns a failure back into progress.
                    if !matches!(state(), State::InstallFailed(_) | State::Installed) {
                        set(s);
                    }
                }
            }),
            Err(e) => {
                crate::log(&format!("update: no bus: {e}"));
                false
            }
        };
        if !sent {
            set(State::InstallFailed(NEEDS_HBCHANNEL.into()));
        }
    }
    #[cfg(windows)]
    set(State::InstallFailed("only possible on the TV".into()));
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn versions() {
        assert!(newer("0.2.62", "0.2.61"));
        assert!(newer("0.2.10", "0.2.9"));
        assert!(newer("0.3", "0.2.99"));
        assert!(newer("v1.0.0", "0.9.9"));
        assert!(!newer("0.2.61", "0.2.61"));
        assert!(!newer("0.2.61", "0.2.62"));
        assert!(!newer("0.2", "0.2.0"));
    }

    #[test]
    fn manifest() {
        let r = parse_manifest(r#"{"version":"0.2.61","ipkUrl":"https://x/a.ipk","ipkHash":{"sha256":"ab"}}"#).unwrap();
        assert_eq!(r, Release { version: "0.2.61".into(), ipk_url: "https://x/a.ipk".into(), sha256: "ab".into() });
        assert!(parse_manifest(r#"{"version":"0.2.61"}"#).is_none());
    }

    #[test]
    fn install_replies() {
        assert_eq!(read_install_reply(r#"{"statusText":"Downloading…","progress":41.7}"#), Some(State::Downloading(Some(41))));
        assert_eq!(read_install_reply(r#"{"statusText":"Downloading…"}"#), Some(State::Downloading(None)));
        assert_eq!(read_install_reply(r#"{"statusText":"Verifying…"}"#), Some(State::Installing));
        assert_eq!(read_install_reply(r#"{"statusText":"Installing…"}"#), Some(State::Installing));
        assert_eq!(read_install_reply(r#"{"statusText":"Finished.","finished":true}"#), Some(State::Installed));
        assert_eq!(
            read_install_reply(r#"{"returnValue":false,"errorText":"Service does not exist: org.webosbrew.hbchannel.service."}"#),
            Some(State::InstallFailed(NEEDS_HBCHANNEL.into()))
        );
        assert_eq!(read_install_reply(r#"{"returnValue":false,"errorText":"Invalid file checksum"}"#), Some(State::InstallFailed("Invalid file checksum".into())));
        assert_eq!(read_install_reply(r#"{"returnValue":true}"#), None);
    }
}
