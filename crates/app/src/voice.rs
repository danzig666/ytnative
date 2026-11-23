//! LG voice search: hold the Magic Remote's mic while ytnative is in front and what you say is
//! searched. `com.webos.service.voiceconductor` routes a mic press to the interactor registered by
//! the app in front, matched by app id, so the registration and the answer go over SDL's own
//! connection, which holds the app id ([`Luna::sdl_app`]); our own bus names never hear of a press,
//! and `LSCallFromApplication` is refused to an app. The session's events and the cancel are
//! private methods, called over the wide connection.
//!
//! Each press: voiceconductor sends `setContext` with a ticket; we subscribe to the session's
//! `getUtteranceEvents`, answer `setContext` with no in-app intents (no answer ends the session at
//! once), take the speech-to-text result (`sttResult`) and cancel LG's own handling of it
//! (`cancelRecognition`, else LG opens its search results over the app), then search it here.
//! Seen on the C3 (webOS 23) 2026-11-22; the same route as FreeMyTV (joaowatanabe/webos-tv-enhancer).

use crate::ls2::Luna;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

const VC: &str = "luna://com.webos.service.voiceconductor";

pub fn start() {
    plx_base::task::spawn("voice", || {
        let (app, wide) = match (Luna::sdl_app(), Luna::wide(plx_base::paths::app_id())) {
            (Ok(a), Ok(w)) => (a, w),
            (a, w) => return crate::log(&format!("voice: no bus: {:?} / {:?}", a.err(), w.err())),
        };
        let ok = app.subscribe(
            &format!("{VC}/interactor/register"),
            r#"{"type":"foreground","subscribe":true}"#,
            // Replies are calls on the same bus: never block the bus thread that delivers this.
            move |msg| {
                let msg = msg.to_string();
                std::thread::spawn(move || on_interactor(app, wide, &msg));
            },
        );
        crate::log(&format!("voice: interactor/register -> {ok}"));
    });
}

fn on_interactor(app: &'static Luna, wide: &'static Luna, msg: &str) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) else { return };
    if v["command"].as_str() != Some("setContext") {
        return crate::log(&format!("voice: {}", msg.chars().take(200).collect::<String>()));
    }
    // The session's events first, so the result is not missed; then the answer that lets it run.
    let done = Arc::new(AtomicBool::new(false));
    let subscribed = wide.subscribe(&format!("{VC}/getUtteranceEvents"), r#"{"subscribe":true}"#, move |m| {
        // Dozens of voice-level events per session: only the result and an error matter.
        if !(m.contains("\"sttResult\"") || m.contains("\"error\"")) {
            return;
        }
        let m = m.to_string();
        let done = done.clone();
        std::thread::spawn(move || on_utterance(wide, &m, &done));
    });
    let p = serde_json::json!({ "voiceTicket": v["voiceTicket"], "inAppIntents": [] });
    let r = app.call(&format!("{VC}/interactor/setContext"), &p.to_string());
    crate::log(&format!("voice: session (events {subscribed}), setContext -> {r}"));
}

fn on_utterance(luna: &'static Luna, msg: &str, done: &AtomicBool) {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(msg) else { return };
    match v["event"].as_str() {
        Some("sttResult") => {
            let Some(query) = first_text(&v) else { return };
            if done.swap(true, Ordering::SeqCst) {
                return;
            }
            let r = luna.call(&format!("{VC}/cancelRecognition"), "{}");
            crate::log(&format!("voice: heard {} candidate(s), cancelRecognition -> {r}", v["text"].as_array().map_or(0, |a| a.len())));
            plx_platform::tv::launch::offer(&serde_json::json!({ "query": query }).to_string());
            plx_machine::present::wake_from_worker();
        }
        Some("error") => crate::log(&format!("voice: not recognised ({})", v["errorCode"])),
        _ => {}
    }
}

// ---- The recognition language ----
//
// LG's recognizer listens in the TV's voice language (`localeInfo.locales.STT`), which the TV's
// settings set from its MENU language: an English menu means English voice search in every app
// that uses it. (LG's YouTube app records the microphone and asks Google itself, so it is not
// bound to it.) While ytnative is in front, the voice language is switched to the interface's
// (and NLP with it, as the settings app sets the two together), and put back when ytnative leaves
// the foreground or quits. The value to put back is kept in a file, so a crash in between is
// repaired at the next start.

const SAVED: &str = "voice-language.json";
static WANTED: std::sync::Mutex<String> = std::sync::Mutex::new(String::new());
static FRONT: AtomicBool = AtomicBool::new(false);
/// One switch at a time, in the order asked.
static SWITCH: std::sync::Mutex<()> = std::sync::Mutex::new(());

/// The interface language changed (its table code: "hu", "zh-TW"; "en").
pub fn set_language(code: &str) {
    let changed = {
        let mut w = WANTED.lock().unwrap();
        let changed = *w != code;
        *w = code.to_string();
        changed
    };
    if changed && FRONT.load(Ordering::SeqCst) {
        std::thread::spawn(apply);
    }
}

/// ytnative came to the front (`true`) or left it, or is quitting (`false`). `wait`: do the
/// switch before returning (at quit).
pub fn foreground(front: bool, wait: bool) {
    FRONT.store(front, Ordering::SeqCst);
    let f = move || if front { apply() } else { restore() };
    if wait {
        // At quit: a slow settings service must not hold the exit for long (a file left behind is
        // put right at the next start).
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            f();
            let _ = tx.send(());
        });
        let _ = rx.recv_timeout(std::time::Duration::from_secs(3));
    } else {
        std::thread::spawn(f);
    }
}

#[cfg(unix)]
fn settings_bus() -> Option<&'static Luna> {
    Luna::wide(plx_base::paths::app_id()).ok()
}

#[cfg(unix)]
fn locale_info(luna: &Luna) -> Option<serde_json::Value> {
    let r = luna.call("luna://com.webos.settingsservice/getSystemSettings", r#"{"keys":["localeInfo"]}"#);
    let v: serde_json::Value = serde_json::from_str(&r).ok()?;
    v["settings"]["localeInfo"]["locales"].is_object().then(|| v["settings"]["localeInfo"].clone())
}

#[cfg(unix)]
fn set_stt(luna: &Luna, mut info: serde_json::Value, stt: &str, nlp: &str) -> bool {
    info["locales"]["STT"] = serde_json::json!(stt);
    info["locales"]["NLP"] = serde_json::json!(nlp);
    let p = serde_json::json!({ "settings": { "localeInfo": info } });
    let r = luna.call("luna://com.webos.settingsservice/setSystemSettings", &p.to_string());
    crate::log(&format!("voice: language {stt} -> {}", r.chars().take(160).collect::<String>()));
    r.contains("\"returnValue\":true")
}

#[cfg(unix)]
fn apply() {
    let _one = SWITCH.lock().unwrap();
    // ytnative left the front after this was asked (a restore may have run already): not now.
    if !FRONT.load(Ordering::SeqCst) {
        return;
    }
    let code = WANTED.lock().unwrap().clone();
    if code.is_empty() {
        return;
    }
    let Some(luna) = settings_bus() else { return };
    let Some(info) = locale_info(luna) else { return crate::log("voice: no localeInfo") };
    let (stt, nlp) = (info["locales"]["STT"].as_str().unwrap_or("").to_string(), info["locales"]["NLP"].as_str().unwrap_or("").to_string());
    let saved_path = yt_core::paths::data_file(SAVED);
    let saved: Option<serde_json::Value> = std::fs::read_to_string(&saved_path).ok().and_then(|s| serde_json::from_str(&s).ok());
    // A voice language of the TV's own since our last switch (the menu language changed): that
    // is the TV's now, and the saved one is stale.
    let saved = saved.filter(|s| s["set"].as_str() == Some(stt.as_str()));
    // The TV's own voice language: kept from our first switch, else the current one.
    let own = saved.as_ref().and_then(|s| s["STT"].as_str()).map(str::to_string).unwrap_or_else(|| stt.clone());
    let r = luna.call("luna://com.webos.service.voiceconductor/getSupportedLanguages", "{}");
    let supported = serde_json::from_str::<serde_json::Value>(&r).ok().map(|v| v["voiceLanguages"].clone()).unwrap_or_default();
    // None: the TV's own language is the interface's, or LG has no recognizer for it.
    match crate::i18n::voice_language(&code, &own, &supported) {
        None => restore_locked(luna),
        Some(target) if target == stt => {}
        Some(target) => {
            let keep = match saved {
                Some(mut s) => {
                    s["set"] = serde_json::json!(target);
                    s
                }
                None => serde_json::json!({ "STT": stt, "NLP": nlp, "set": target }),
            };
            if std::fs::write(&saved_path, keep.to_string()).is_err() {
                return crate::log("voice: cannot save the TV's voice language; not switching");
            }
            set_stt(luna, info, &target, &target);
        }
    }
}

#[cfg(unix)]
fn restore() {
    let _one = SWITCH.lock().unwrap();
    if let Some(luna) = settings_bus() {
        restore_locked(luna);
    }
}

/// Puts the TV's own voice language back (with [`SWITCH`] held).
#[cfg(unix)]
fn restore_locked(luna: &Luna) {
    let saved_path = yt_core::paths::data_file(SAVED);
    let Some(saved) = std::fs::read_to_string(&saved_path).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) else { return };
    let Some(info) = locale_info(luna) else { return };
    let (stt, nlp) = (saved["STT"].as_str().unwrap_or(""), saved["NLP"].as_str().unwrap_or(""));
    // Put back only what is still ours: if the menu language was changed meanwhile, the TV has
    // set a voice language of its own.
    let ours = info["locales"]["STT"].as_str() == saved["set"].as_str();
    if ours && !stt.is_empty() && !set_stt(luna, info, stt, if nlp.is_empty() { stt } else { nlp }) {
        return; // the file stays: tried again at the next start
    }
    let _ = std::fs::remove_file(&saved_path);
}

#[cfg(not(unix))]
fn apply() {}
#[cfg(not(unix))]
fn restore() {}

/// The best speech-to-text candidate of an `sttResult` (`"text": ["Captain", "Capital"]`).
fn first_text(v: &serde_json::Value) -> Option<String> {
    v["text"].as_array()?.iter().filter_map(|t| t.as_str()).map(str::trim).find(|t| !t.is_empty()).map(String::from)
}

#[cfg(test)]
mod tests {
    #[test]
    fn the_best_candidate_is_searched() {
        let v = serde_json::json!({ "event": "sttResult", "text": ["  ", "Captain", "Capital"] });
        assert_eq!(super::first_text(&v).as_deref(), Some("Captain"));
        assert_eq!(super::first_text(&serde_json::json!({ "event": "sttResult" })), None);
    }
}
