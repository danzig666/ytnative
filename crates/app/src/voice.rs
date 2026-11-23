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

// ---- Repair of an earlier build's voice-language switch ----
//
// A build of 2026-11-23 switched the TV's voice language (`localeInfo.locales.STT` / `NLP`) to
// ytnative's interface language while ytnative was in front. It cannot work: LG's voice service
// derives STT from the MENU language and resets it at once, so recognition stayed in the menu's
// language, and only NLP kept the switched value. That build kept the TV's own values in
// `voice-language.json`; at start, every value still on the one it set is put back, and the file
// goes.

const SAVED: &str = "voice-language.json";

/// Puts back what the earlier switch left (see above). Once, in the background.
#[cfg(unix)]
pub fn repair() {
    let path = yt_core::paths::data_file(SAVED);
    let Some(saved) = std::fs::read_to_string(&path).ok().and_then(|s| serde_json::from_str::<serde_json::Value>(&s).ok()) else { return };
    plx_base::task::spawn("voice-repair", move || {
        let Ok(luna) = Luna::wide(plx_base::paths::app_id()) else { return };
        let r = luna.call("luna://com.webos.settingsservice/getSystemSettings", r#"{"keys":["localeInfo"]}"#);
        let Some(mut info) = serde_json::from_str::<serde_json::Value>(&r).ok().map(|v| v["settings"]["localeInfo"].clone()).filter(|i| i["locales"].is_object()) else { return };
        let set = saved["set"].as_str().unwrap_or("");
        let mut changed = false;
        for k in ["STT", "NLP"] {
            let own = saved[k].as_str().unwrap_or("");
            if !set.is_empty() && !own.is_empty() && info["locales"][k].as_str() == Some(set) {
                info["locales"][k] = serde_json::json!(own);
                changed = true;
            }
        }
        if changed {
            let p = serde_json::json!({ "settings": { "localeInfo": info } });
            let r = luna.call("luna://com.webos.settingsservice/setSystemSettings", &p.to_string());
            crate::log(&format!("voice: the TV's voice language put back -> {}", r.chars().take(160).collect::<String>()));
            if !r.contains("\"returnValue\":true") {
                return; // the file stays: tried again at the next start
            }
        }
        let _ = std::fs::remove_file(&path);
    });
}

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
