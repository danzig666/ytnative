//! 2019 Realtek LCD TVs (board `k5lp`/`k3lp`, e.g. UM7100, UM7400, UM7600; issue #8): the
//! Developer Mode jail the app runs in has no `/dev/rtkmem`, and the TV's video path aborts the app
//! (SIGABRT) a moment after a video starts.
//!
//! Two ways to add the device, tried in this order the first time a video is opened:
//! 1. Root (Homebrew Channel's `exec`): rebuild the jail with the `native` profile, which has the
//!    device; the running app sees it at once (the node appears in the jail's own `/dev`). The
//!    PlxNative repair (`platform/src/webos/jail_repair.rs`), from webosbrew PR #202.
//! 2. No root: LG's own, LG-signed jail config. The jailer reads `/media/developer/jail_app.conf`
//!    (+ `.sig`, checked against LG's app-signing certificate) on top of the firmware's
//!    `/etc/jail_native_devmode.conf`; LG's current file has `copynod /dev/rtkmem` for Realtek
//!    sets, the firmware's copy on these TVs does not. LG's Developer Mode app normally downloads
//!    it; rooted TVs often lack it. Downloaded from developer.lge.com for this webOS release, the
//!    way mariotaku's Kodi add-on (kodi.addon.webos-jailer-fix) does, and used after the TV has
//!    been unplugged for a few seconds. A Developer Mode app may write there: it is the jail's
//!    `$HOME`. Existing files are never replaced.
//!
//! The check runs at boot (log line `devjail: soc=… rtkmem=…`); a video on a TV that lacks the
//! device does not start (it would close the app) and the player says what to do.

use std::path::Path;
use std::sync::atomic::{AtomicU8, Ordering};

const RTKMEM: &str = "/dev/rtkmem";
const OK_MARKER: &str = "YTNATIVE_JAIL_REPAIR_OK";
const NOT_ROOT_MARKER: &str = "YTNATIVE_JAIL_REPAIR_NOT_ROOT";
const LG_CONF_DIR: &str = "/media/developer";

const UNKNOWN: u8 = 0;
const FINE: u8 = 1; // not an affected TV, or the device is there
const MISSING: u8 = 2;
const REPAIRING: u8 = 3;
const UNPLUG: u8 = 4; // LG's jail config is in place: it applies after the TV was unplugged
const NO_FIX: u8 = 5; // neither way worked in this run of the app

static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);
/// LG's files were already there before this run (so unplugging the TV did not help, or was not
/// done yet).
static HAD_LG_CONF: AtomicU8 = AtomicU8::new(0);

/// Boot: is this an affected TV without the device?
pub fn probe() {
    let name = std::fs::read_to_string("/etc/prefs/properties/machineName").map(|s| s.trim().to_string()).unwrap_or_default();
    let affected = plx_platform::webos::is_realtek_k5lp_family(&name);
    let missing = affected && !readable();
    STATE.store(if missing { MISSING } else { FINE }, Ordering::SeqCst);
    let lg = has_lg_conf(Path::new(LG_CONF_DIR));
    HAD_LG_CONF.store(lg as u8, Ordering::SeqCst);
    crate::log(&format!(
        "devjail: soc={name} rtkmem={} lg_conf={}",
        if !affected { "n/a" } else if missing { "missing" } else { "ok" },
        if lg { "present" } else { "absent" }
    ));
}

#[cfg(not(windows))]
fn readable() -> bool {
    let p = std::ffi::CString::new(RTKMEM).unwrap();
    unsafe { libc::access(p.as_ptr(), libc::R_OK) == 0 }
}

/// The simulator (built so the tests below run on Windows CI) has no jail.
#[cfg(windows)]
fn readable() -> bool {
    true
}

/// Before a video: `None` when it may play; otherwise why not, with the fix started in the
/// background the first time.
pub fn blocks_video() -> Option<&'static str> {
    match STATE.load(Ordering::SeqCst) {
        MISSING | UNPLUG | NO_FIX if readable() => {
            STATE.store(FINE, Ordering::SeqCst);
            None
        }
        // Tried again on every press until one way worked (a download can fail on a bad network).
        s @ (MISSING | NO_FIX) => {
            STATE.store(REPAIRING, Ordering::SeqCst);
            let started = plx_base::task::spawn("jail-repair", || {
                let state = fix();
                STATE.store(state, Ordering::SeqCst);
                plx_machine::present::wake_from_worker();
            });
            if started.is_none() {
                STATE.store(s, Ordering::SeqCst);
            }
            Some(if s == NO_FIX {
                NO_FIX_TEXT
            } else {
                "This TV's sandbox does not give ytnative /dev/rtkmem, which its video needs, so the video would close the app. \
                 ytnative is fixing it now: open the video again in a few seconds."
            })
        }
        REPAIRING => Some("ytnative is fixing its sandbox so videos can play: open the video again in a few seconds."),
        UNPLUG if HAD_LG_CONF.load(Ordering::SeqCst) == 1 => Some(UNPLUG_AGAIN_TEXT),
        UNPLUG => Some(UNPLUG_TEXT),
        _ => None,
    }
}

const UNPLUG_TEXT: &str = "One more step so videos can play on this TV: ytnative installed LG's own sandbox settings for \
    Developer Mode apps, which add the device video needs (/dev/rtkmem). They take effect after a full restart: turn the TV off, \
    unplug it for 15 seconds, plug it back in and turn it on. If videos still do not play, delete ytnative and install it again \
    the way you installed it before: the app's sandbox is made when the app is installed.";

const UNPLUG_AGAIN_TEXT: &str = "Videos cannot play yet: LG's sandbox settings for Developer Mode apps are installed, but this \
    app's sandbox still lacks /dev/rtkmem. The sandbox is made when the app is installed: delete ytnative and install it again \
    the way you installed it before. If that does not help, turn the TV off, unplug it for 15 seconds and turn it on again. If \
    videos still do not play, please report it in issue #8 on ytnative's GitHub page.";

const NO_FIX_TEXT: &str = "Videos cannot play on this TV yet: its sandbox lacks /dev/rtkmem, which video needs, and ytnative \
    could not add it last time (no root, and LG's sandbox settings could not be downloaded). ytnative is trying again: open the \
    video again in a few seconds. If this keeps coming back, see issue #8 on ytnative's GitHub page.";

/// Worker: root first (works at once), else LG's jail config (after an unplug).
fn fix() -> u8 {
    let r = repair();
    crate::log(&format!("devjail: root repair -> {r:?}"));
    match r {
        Ok(()) => FINE,
        Err(_) if has_lg_conf(Path::new(LG_CONF_DIR)) => UNPLUG,
        Err(_) => {
            let r = install_lg_conf(Path::new(LG_CONF_DIR));
            crate::log(&format!("devjail: LG jail config -> {r:?}"));
            if r.is_ok() {
                UNPLUG
            } else {
                NO_FIX
            }
        }
    }
}

#[derive(Debug)]
#[allow(dead_code)] // the fields are read by the log line's Debug
enum Failure {
    NoHomebrewChannel,
    NotRoot,
    Unsupported,
    Failed(String),
}

fn repair() -> Result<(), Failure> {
    let id = plx_base::paths::app_id();
    let dir = std::env::current_exe().ok().and_then(|e| e.parent().map(|d| d.to_string_lossy().into_owned())).unwrap_or_default();
    let cmd = command(&dir, id).ok_or(Failure::Unsupported)?;
    let luna = crate::ls2::Luna::wide(id).map_err(Failure::Failed)?;
    let reply = luna.call("luna://org.webosbrew.hbchannel.service/exec", &serde_json::json!({ "command": cmd }).to_string());
    match outcome(&reply) {
        Ok(()) if readable() => Ok(()),
        Ok(()) => Err(Failure::Failed("repaired, but /dev/rtkmem is still not readable".into())),
        e => e,
    }
}

/// The root shell command, for this app's own install only (both interpolated values checked).
fn command(dir: &str, id: &str) -> Option<String> {
    let id_ok = !id.is_empty() && id.bytes().all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'_' | b'-'));
    let dir_ok = dir == format!("/media/developer/apps/usr/palm/applications/{id}") || dir == format!("/media/cryptofs/apps/usr/palm/applications/{id}");
    (id_ok && dir_ok).then(|| {
        format!(
            "if [ \"$(id -u)\" != 0 ]; then printf '{NOT_ROOT_MARKER}'; elif [ ! -c {RTKMEM} ]; then exit 74; \
             elif /usr/bin/jailer -t native -p '{dir}' -i '{id}' /bin/true >/dev/null 2>&1 && [ -c '/var/palm/jail/{id}/dev/rtkmem' ]; \
             then printf '{OK_MARKER}'; else exit 74; fi"
        )
    })
}

fn outcome(reply: &str) -> Result<(), Failure> {
    let v: serde_json::Value = serde_json::from_str(reply).map_err(|_| Failure::Failed(reply.chars().take(120).collect()))?;
    if v["returnValue"].as_bool() != Some(true) {
        let text = v["errorText"].as_str().unwrap_or("").to_string();
        return Err(if text.contains("Service does not exist") { Failure::NoHomebrewChannel } else { Failure::Failed(text) });
    }
    match v["stdoutString"].as_str() {
        Some(OK_MARKER) => Ok(()),
        Some(NOT_ROOT_MARKER) => Err(Failure::NotRoot),
        other => Err(Failure::Failed(format!("unexpected output {other:?}"))),
    }
}

fn has_lg_conf(dir: &Path) -> bool {
    ["jail_app.conf", "jail_app.conf.sig"].iter().all(|f| std::fs::metadata(dir.join(f)).is_ok_and(|m| m.len() > 0))
}

/// The webOS release LG's download is keyed by (`webos_release` in nyx's os_info.json, e.g. 4.10.2).
fn webos_release() -> Option<String> {
    let v: serde_json::Value = serde_json::from_str(&std::fs::read_to_string("/var/run/nyx/os_info.json").ok()?).ok()?;
    let r = v["webos_release"].as_str()?.trim();
    (!r.is_empty() && r.bytes().all(|c| c.is_ascii_digit() || c == b'.')).then(|| r.to_string())
}

/// Downloads LG's jail config and its signature for this release into `dir`, both or neither,
/// never over existing files.
fn install_lg_conf(dir: &Path) -> anyhow::Result<()> {
    use anyhow::Context;
    let release = webos_release().context("webOS release unknown")?;
    let http = yt_core::net::builder().timeout(std::time::Duration::from_secs(20)).build()?;
    let get = |kind: &str| -> anyhow::Result<Vec<u8>> {
        let url = format!("https://developer.lge.com/common/file/DownloadFile.dev?sdkVersion={release}&fileType={kind}");
        let r = http.get(&url).send()?.error_for_status()?;
        Ok(r.bytes()?.to_vec())
    };
    let (conf, sig) = (get("conf")?, get("sig")?);
    check_lg_conf(&conf, &sig)?;
    write_lg_conf(dir, &conf, &sig)
}

/// The download looks like LG's jail config (not an error page) and adds the device.
fn check_lg_conf(conf: &[u8], sig: &[u8]) -> anyhow::Result<()> {
    let text = std::str::from_utf8(conf).map_err(|_| anyhow::anyhow!("jail config is not text"))?;
    anyhow::ensure!(text.contains("copynod /dev/rtkmem"), "LG's jail config has no /dev/rtkmem ({} bytes)", conf.len());
    anyhow::ensure!(sig.starts_with(b"-----BEGIN PKCS7-----"), "LG's signature download is not a signature ({} bytes)", sig.len());
    Ok(())
}

fn write_lg_conf(dir: &Path, conf: &[u8], sig: &[u8]) -> anyhow::Result<()> {
    anyhow::ensure!(!dir.join("jail_app.conf").exists() && !dir.join("jail_app.conf.sig").exists(), "LG's jail config is already there");
    let pid = std::process::id();
    let (tc, ts) = (dir.join(format!(".jail_app.conf.{pid}.tmp")), dir.join(format!(".jail_app.conf.sig.{pid}.tmp")));
    let done = (|| -> std::io::Result<()> {
        std::fs::write(&tc, conf)?;
        std::fs::write(&ts, sig)?;
        // The signature first: the jailer ignores a config without one.
        std::fs::rename(&ts, dir.join("jail_app.conf.sig"))?;
        std::fs::rename(&tc, dir.join("jail_app.conf"))
    })();
    let _ = (std::fs::remove_file(&tc), std::fs::remove_file(&ts));
    if done.is_err() {
        let _ = std::fs::remove_file(dir.join("jail_app.conf.sig"));
    }
    Ok(done?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_this_apps_install_is_repaired() {
        let id = "io.github.danzig666.ytnative";
        let c = command(&format!("/media/developer/apps/usr/palm/applications/{id}"), id).unwrap();
        assert!(c.contains("jailer -t native -p '/media/developer/apps/usr/palm/applications/io.github.danzig666.ytnative' -i 'io.github.danzig666.ytnative'"));
        assert!(command("/tmp/x", id).is_none());
        assert!(command("/media/developer/apps/usr/palm/applications/a';reboot;'", "a';reboot;'").is_none());
    }

    #[test]
    fn the_reply_says_whether_it_worked() {
        let ok = serde_json::json!({ "returnValue": true, "stdoutString": OK_MARKER }).to_string();
        assert!(outcome(&ok).is_ok());
        let not_root = serde_json::json!({ "returnValue": true, "stdoutString": NOT_ROOT_MARKER }).to_string();
        assert!(matches!(outcome(&not_root), Err(Failure::NotRoot)));
        let absent = serde_json::json!({ "returnValue": false, "errorText": "Service does not exist: org.webosbrew.hbchannel.service." }).to_string();
        assert!(matches!(outcome(&absent), Err(Failure::NoHomebrewChannel)));
        assert!(outcome("<timeout>").is_err());
    }

    #[test]
    fn only_lgs_config_with_the_device_is_written_and_never_over_a_file() {
        let d = std::env::temp_dir().join(format!("ytnative-jail-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&d);
        std::fs::create_dir_all(&d).unwrap();
        let conf = b"if $rtk == true\n    copynod /dev/rtkmem\n";
        assert!(check_lg_conf(b"<html>error</html>", b"sig").is_err());
        assert!(check_lg_conf(conf, b"<html>").is_err());
        check_lg_conf(conf, b"-----BEGIN PKCS7-----\nMIIE").unwrap();
        assert!(!has_lg_conf(&d));
        write_lg_conf(&d, conf, b"sig").unwrap();
        assert!(has_lg_conf(&d));
        assert_eq!(std::fs::read(d.join("jail_app.conf")).unwrap(), conf);
        assert!(write_lg_conf(&d, b"other", b"other").is_err());
        assert_eq!(std::fs::read(d.join("jail_app.conf")).unwrap(), conf);
        assert_eq!(std::fs::read_dir(&d).unwrap().count(), 2, "no temp files left");
        std::fs::remove_dir_all(&d).unwrap();
    }
}
