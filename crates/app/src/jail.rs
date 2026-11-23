//! 2019 Realtek TVs (board `k5lp`/`k3lp`, e.g. UM7100, UM7600; issue #8): the Developer Mode
//! jail the app runs in has no `/dev/rtkmem`, and the TV's video path aborts the app (SIGABRT) a
//! moment after a video starts. Homebrew Channel's root `exec` rebuilds the jail with the `native`
//! profile, which has the device; the running app sees it at once (the node appears in the jail's
//! own `/dev`). The same check and repair as PlxNative (`platform/src/webos/jail_repair.rs`), from
//! webosbrew/webos-homebrew-channel PR #202.
//!
//! The check runs at boot (log line `devjail: soc=… rtkmem=…`); a video on a TV that lacks the
//! device does not start but repairs the jail first (rooted TVs only) and tells the viewer.

use std::sync::atomic::{AtomicU8, Ordering};

const RTKMEM: &str = "/dev/rtkmem";
const OK_MARKER: &str = "YTNATIVE_JAIL_REPAIR_OK";
const NOT_ROOT_MARKER: &str = "YTNATIVE_JAIL_REPAIR_NOT_ROOT";

const UNKNOWN: u8 = 0;
const FINE: u8 = 1; // not an affected TV, or the device is there
const MISSING: u8 = 2;
const REPAIRING: u8 = 3;

static STATE: AtomicU8 = AtomicU8::new(UNKNOWN);

/// Boot: is this an affected TV without the device?
pub fn probe() {
    let name = std::fs::read_to_string("/etc/prefs/properties/machineName").map(|s| s.trim().to_string()).unwrap_or_default();
    let affected = plx_platform::webos::is_realtek_k5lp_family(&name);
    let missing = affected && !readable();
    STATE.store(if missing { MISSING } else { FINE }, Ordering::SeqCst);
    crate::log(&format!("devjail: soc={name} rtkmem={}", if !affected { "n/a" } else if missing { "missing" } else { "ok" }));
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

/// Before a video: `None` when it may play; otherwise why not, with a repair started in the
/// background the first time (the next press plays once it worked).
pub fn blocks_video() -> Option<&'static str> {
    match STATE.load(Ordering::SeqCst) {
        MISSING if readable() => {
            STATE.store(FINE, Ordering::SeqCst);
            None
        }
        MISSING => {
            STATE.store(REPAIRING, Ordering::SeqCst);
            let started = plx_base::task::spawn("jail-repair", || {
                let r = repair();
                crate::log(&format!("devjail: repair -> {r:?}"));
                STATE.store(if r.is_ok() { FINE } else { MISSING }, Ordering::SeqCst);
                plx_machine::present::wake_from_worker();
            });
            if started.is_none() {
                STATE.store(MISSING, Ordering::SeqCst);
            }
            Some(
                "This TV's sandbox does not give ytnative /dev/rtkmem, which its video needs, so the video would close the app. \
                 ytnative is repairing it now (rooted TVs with Homebrew Channel): open the video again in a few seconds. \
                 Without root, see issue #8 on ytnative's GitHub page.",
            )
        }
        REPAIRING => Some("Repairing ytnative's sandbox so videos can play: open the video again in a few seconds."),
        _ => None,
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
        Ok(()) => Err(Failure::Failed("repaired, but /dev/rtkmem is still not readable: close and reopen ytnative".into())),
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
}
